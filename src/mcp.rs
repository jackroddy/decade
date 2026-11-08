use std::io;
use std::net::TcpListener;

use decade_channel::{Focus, code};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use smithay::reexports::calloop::channel::Sender;
use tokio::sync::oneshot;

use crate::scratch::Launch;

pub struct Request {
    /// The workspace named in the address the call came in on.
    pub workspace: u32,
    pub call: Call,

    /// The tool result's text, or the reason the call failed.
    pub reply: oneshot::Sender<Result<String, String>>,
}

pub enum Call {
    /// A pane verb for the program in pane `to`.
    Send {
        to: String,
        verb: String,
        payload: Value,
    },

    /// A pane verb for the program in the scratch pane.
    ScratchSend {
        verb: String,
        payload: Value,
    },
    Focus(String),
    Panes,
    ScratchOpen(Launch),
}

/// Start the MCP server on a thread and return its URL.
pub fn start(requests: Sender<Request>) -> io::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let url = format!("http://127.0.0.1:{port}/mcp");
    listener.set_nonblocking(true)?;
    std::thread::Builder::new()
        .name("decade-mcp".into())
        .spawn(move || serve(listener, requests))?;
    Ok(url)
}

fn serve(listener: TcpListener, requests: Sender<Request>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime for the MCP server");
    let served = runtime.block_on(async move {
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let service: StreamableHttpService<Decade, LocalSessionManager> =
            StreamableHttpService::new(
                move || {
                    Ok(Decade {
                        requests: requests.clone(),
                    })
                },
                Default::default(),
                StreamableHttpServerConfig::default(),
            );
        let router = axum::Router::new().nest_service("/mcp", service);
        axum::serve(listener, router).await
    });
    if let Err(error) = served {
        eprintln!("decade: the MCP server stopped: {error}");
    }
}

#[derive(Clone)]
struct Decade {
    requests: Sender<Request>,
}

fn schema(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn tools() -> Vec<Tool> {
    let path = json!({"type": "string", "description": "a file path, absolute or relative to decade's working directory"});
    vec![
        Tool::new(
            "code.show",
            "Show a file in the code pane, scrolled to a line if one is given",
            schema(json!({
                "type": "object",
                "properties": {
                    "path": path,
                    "line": {"type": "integer", "minimum": 1, "description": "the line to centre on"}
                },
                "required": ["path"]
            })),
        ),
        Tool::new(
            "code.mark",
            "Highlight ranges of lines in a file in the code pane; the file is shown if it is not already",
            schema(json!({
                "type": "object",
                "properties": {
                    "path": path,
                    "lines": {
                        "type": "array",
                        "description": "inclusive [from, to] line ranges, 1-based",
                        "items": {"type": "array", "items": {"type": "integer", "minimum": 1}, "minItems": 2, "maxItems": 2}
                    }
                },
                "required": ["path", "lines"]
            })),
        ),
        Tool::new(
            "code.clear",
            "Remove every highlight from the code pane",
            schema(json!({"type": "object", "properties": {}})),
        ),
        Tool::new(
            "focus",
            "Move keyboard focus to a pane",
            schema(json!({
                "type": "object",
                "properties": {"pane": {"type": "string", "description": "a pane name, as panes lists it"}},
                "required": ["pane"]
            })),
        ),
        Tool::new(
            "panes",
            "List the panes in this workspace and the program occupying each",
            schema(json!({"type": "object", "properties": {}})),
        ),
        Tool::new(
            "scratch.open",
            "Start a decade- program in the scratch pane, closing the program there first",
            schema(json!({
                "type": "object",
                "properties": {
                    "program": {"type": "string", "description": "a program on decade's PATH whose name starts with decade-"},
                    "args": {"type": "array", "items": {"type": "string"}, "description": "the program's arguments"},
                    "terminal": {"type": "boolean", "description": "run it in a terminal, the default, or as its own window"}
                },
                "required": ["program"]
            })),
        ),
        Tool::new(
            "scratch.send",
            "Send a message to the program in the scratch pane",
            schema(json!({
                "type": "object",
                "properties": {
                    "verb": {"type": "string", "description": "the message's verb"},
                    "payload": {"type": "object", "description": "the message's payload"}
                },
                "required": ["verb", "payload"]
            })),
        ),
    ]
}

fn workspace_in(query: &str) -> Option<u32> {
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix("workspace="))
        .and_then(|id| id.parse().ok())
}

/// The arguments of `scratch.send`.
#[derive(Deserialize)]
struct ScratchSend {
    verb: String,
    payload: Map<String, Value>,
}

fn call_for(name: &str, arguments: Map<String, Value>) -> Result<Call, String> {
    let code = |verb: &str, payload: Map<String, Value>| Call::Send {
        to: "code".to_string(),
        verb: verb.to_string(),
        payload: Value::Object(payload),
    };
    match name {
        "code.show" => Ok(code(code::SHOW, arguments)),
        "code.mark" => Ok(code(code::MARK, arguments)),
        "code.clear" => Ok(code(code::CLEAR, Map::new())),
        "focus" => {
            let Focus { pane } = parse(name, arguments)?;
            Ok(Call::Focus(pane))
        }
        "panes" => Ok(Call::Panes),
        "scratch.open" => parse(name, arguments).map(Call::ScratchOpen),
        "scratch.send" => {
            let ScratchSend { verb, payload } = parse(name, arguments)?;
            Ok(Call::ScratchSend {
                verb,
                payload: Value::Object(payload),
            })
        }
        _ => Err(format!("no tool named {name}")),
    }
}

fn parse<T: DeserializeOwned>(tool: &str, arguments: Map<String, Value>) -> Result<T, String> {
    serde_json::from_value(Value::Object(arguments)).map_err(|error| format!("{tool}: {error}"))
}

impl ServerHandler for Decade {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("decade", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "decade is the window the user is working in. These tools reach the panes of the \
                 workspace this agent was started in, and panes lists them. A pane named code \
                 shows one file; use code.show to bring a file into view and code.mark to point \
                 at lines in it. The user may mark one pane as the scratch pane, where agents \
                 show things. scratch.open starts a program whose name begins with decade- in \
                 it, replacing the program there, and scratch.send sends that program a message. \
                 Both fail when the workspace has no scratch pane.",
            )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let workspace = context
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|parts| parts.uri.query())
            .and_then(workspace_in)
            .ok_or_else(|| ErrorData::invalid_params("the MCP address names no workspace", None))?;
        let call = call_for(&request.name, request.arguments.unwrap_or_default())
            .map_err(|reason| ErrorData::invalid_params(reason, None))?;
        let (reply, answer) = oneshot::channel();
        self.requests
            .send(Request {
                workspace,
                call,
                reply,
            })
            .map_err(|_| ErrorData::internal_error("decade's event loop is gone", None))?;
        let answer = answer
            .await
            .map_err(|_| ErrorData::internal_error("decade did not answer", None))?;
        let result = match answer {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            Err(reason) => CallToolResult::error(vec![ContentBlock::text(reason)]),
        };
        Ok(result.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::ServiceExt;
    use rmcp::transport::StreamableHttpClientTransport;
    use smithay::reexports::calloop::EventLoop;
    use smithay::reexports::calloop::channel::{self, Event};
    use std::time::Duration;

    #[test]
    fn the_workspace_is_read_from_the_query_of_the_address() {
        assert_eq!(workspace_in("workspace=3"), Some(3));
        assert_eq!(workspace_in("a=b&workspace=12"), Some(12));
        assert_eq!(workspace_in("workspace=x"), None);
        assert_eq!(workspace_in("other=1"), None);
    }

    #[test]
    fn tools_are_relayed_as_calls_and_answers_become_results() {
        let (sender, source) = channel::channel::<Request>();
        let unaddressed = start(sender).unwrap();
        let url = format!("{unaddressed}?workspace=7");

        let mut event_loop: EventLoop<Vec<(u32, Value)>> = EventLoop::try_new().unwrap();
        event_loop
            .handle()
            .insert_source(source, |event, _, seen| {
                let Event::Msg(request) = event else {
                    return;
                };
                let reply = match request.call {
                    Call::Send { to, verb, payload } => {
                        seen.push((request.workspace, json!({"to": to, verb: payload})));
                        Ok("sent".to_string())
                    }
                    Call::Panes => {
                        Ok(r#"{"panes":[{"name":"code","client":"decade-code"}]}"#.to_string())
                    }
                    Call::Focus(pane) => Err(format!("no pane named {pane}")),
                    _ => Err("unexpected call".to_string()),
                };
                let _ = request.reply.send(reply);
            })
            .unwrap();

        let client = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let client = ().serve(StreamableHttpClientTransport::from_uri(url)).await.unwrap();
                let names: Vec<String> = client
                    .list_all_tools()
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|tool| tool.name.to_string())
                    .collect();
                let show = client
                    .call_tool(
                        CallToolRequestParams::new("code.show")
                            .with_arguments(schema(json!({"path": "src/main.rs", "line": 3}))),
                    )
                    .await
                    .unwrap();
                let panes = client
                    .call_tool(CallToolRequestParams::new("panes"))
                    .await
                    .unwrap();
                let focus = client
                    .call_tool(
                        CallToolRequestParams::new("focus")
                            .with_arguments(schema(json!({"pane": "terminal"}))),
                    )
                    .await
                    .unwrap();
                client.cancel().await.unwrap();
                let unaddressed =
                    ().serve(StreamableHttpClientTransport::from_uri(unaddressed))
                        .await
                        .unwrap();
                let refused = unaddressed
                    .call_tool(CallToolRequestParams::new("panes"))
                    .await;
                unaddressed.cancel().await.unwrap();
                (names, show, panes, focus, refused)
            })
        });

        let mut seen = Vec::new();
        while !client.is_finished() {
            event_loop
                .dispatch(Some(Duration::from_millis(50)), &mut seen)
                .unwrap();
        }
        let (names, show, panes, focus, refused) = client.join().unwrap();

        assert_eq!(
            names,
            [
                "code.show",
                "code.mark",
                "code.clear",
                "focus",
                "panes",
                "scratch.open",
                "scratch.send"
            ]
        );
        assert_eq!(
            seen,
            [(
                7,
                json!({"to": "code", "show": {"path": "src/main.rs", "line": 3}})
            )]
        );
        assert_eq!(show.is_error, Some(false));
        assert_eq!(show.content[0].as_text().unwrap().text, "sent");
        assert_eq!(panes.is_error, Some(false));
        assert!(
            panes.content[0]
                .as_text()
                .unwrap()
                .text
                .contains("decade-code")
        );
        assert_eq!(focus.is_error, Some(true));
        assert!(
            focus.content[0]
                .as_text()
                .unwrap()
                .text
                .contains("no pane named terminal")
        );
        assert!(refused.is_err());
    }

    #[test]
    fn tool_arguments_take_their_defaults_and_wrong_types_are_refused() {
        let Ok(Call::ScratchOpen(launch)) =
            call_for("scratch.open", schema(json!({"program": "decade-code"})))
        else {
            panic!("scratch.open with a program is a launch");
        };
        assert_eq!(launch.program, "decade-code");
        assert!(launch.args.is_empty());
        assert!(launch.terminal);
        let args = schema(json!({"program": "decade-code", "args": [1]}));
        let Err(reason) = call_for("scratch.open", args) else {
            panic!("a number is not an argument");
        };
        assert!(reason.starts_with("scratch.open: invalid type: integer"));
        let windowed = json!({"program": "decade-code", "args": ["a"], "terminal": false});
        let Ok(Call::ScratchOpen(launch)) = call_for("scratch.open", schema(windowed)) else {
            panic!("scratch.open with a program is a launch");
        };
        assert_eq!(launch.args, ["a"]);
        assert!(!launch.terminal);
        let payload = schema(json!({"verb": "show", "payload": "x"}));
        let Err(reason) = call_for("scratch.send", payload) else {
            panic!("a string is not a payload");
        };
        assert!(reason.starts_with("scratch.send: invalid type: string"));
        let missing = call_for("focus", Map::new());
        assert_eq!(missing.err().unwrap(), "focus: missing field `pane`");
    }
}
