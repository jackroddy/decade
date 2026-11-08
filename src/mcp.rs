use std::io;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::{Map, Value, json};
use smithay::reexports::calloop::channel::Sender;
use tokio::sync::oneshot;

use crate::scratch::Launch;

pub struct Request {
    /// The workspace named in the address the call came in on.
    pub workspace: Option<u32>,
    pub call: Call,
    pub reply: oneshot::Sender<Vec<String>>,
}

pub enum Call {
    /// A socket line from the built-in agent client.
    Line(String),
    ScratchOpen(Launch),

    /// A message to the program in the scratch pane.
    ScratchSend {
        verb: String,
        payload: Value,
    },
}

pub struct Server {
    pub url: String,
}

pub fn start(requests: Sender<Request>) -> io::Result<Server> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let url = format!("http://127.0.0.1:{port}/mcp");
    listener.set_nonblocking(true)?;
    let requests = Arc::new(Mutex::new(requests));
    std::thread::Builder::new()
        .name("decade-mcp".into())
        .spawn(move || serve(listener, requests))?;
    Ok(Server { url })
}

fn serve(listener: TcpListener, requests: Arc<Mutex<Sender<Request>>>) {
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
    requests: Arc<Mutex<Sender<Request>>>,
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

fn call_for(name: &str, mut arguments: Map<String, Value>) -> Result<Call, String> {
    let line = match name {
        "code.show" => json!({"to": "code", "show": arguments}),
        "code.mark" => json!({"to": "code", "mark": arguments}),
        "code.clear" => json!({"to": "code", "clear": {}}),
        "focus" => json!({"focus": arguments}),
        "panes" => json!({"panes": {}}),
        "scratch.open" => return scratch_open(arguments).map(Call::ScratchOpen),
        "scratch.send" => {
            let Some(Value::String(verb)) = arguments.remove("verb") else {
                return Err("scratch.send needs a verb, a string".to_string());
            };
            let Some(payload @ Value::Object(_)) = arguments.remove("payload") else {
                return Err("scratch.send needs a payload, an object".to_string());
            };
            return Ok(Call::ScratchSend { verb, payload });
        }
        _ => return Err(format!("no tool named {name}")),
    };
    Ok(Call::Line(line.to_string()))
}

fn scratch_open(arguments: Map<String, Value>) -> Result<Launch, String> {
    let Some(Value::String(program)) = arguments.get("program") else {
        return Err("scratch.open needs a program, a string".to_string());
    };
    let args = match arguments.get("args") {
        None => Vec::new(),
        Some(Value::Array(args)) => args
            .iter()
            .map(|arg| arg.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
            .ok_or("scratch.open's args must be strings")?,
        Some(_) => return Err("scratch.open's args must be an array".to_string()),
    };
    let terminal = match arguments.get("terminal") {
        None => true,
        Some(Value::Bool(terminal)) => *terminal,
        Some(_) => return Err("scratch.open's terminal must be true or false".to_string()),
    };
    Ok(Launch {
        program: program.clone(),
        args,
        terminal,
    })
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
            .and_then(workspace_in);
        let call = call_for(&request.name, request.arguments.unwrap_or_default())
            .map_err(|reason| ErrorData::invalid_params(reason, None))?;
        let (reply, answer) = oneshot::channel();
        self.requests
            .lock()
            .unwrap()
            .send(Request {
                workspace,
                call,
                reply,
            })
            .map_err(|_| ErrorData::internal_error("decade's event loop is gone", None))?;
        let answer = answer
            .await
            .map_err(|_| ErrorData::internal_error("decade did not answer", None))?;
        let failed = answer.iter().any(|text| {
            serde_json::from_str::<Value>(text)
                .map(|value| value.get("error").is_some())
                .unwrap_or(false)
        });
        let content = if answer.is_empty() {
            vec![ContentBlock::text(format!(
                "sent {} to decade",
                request.name
            ))]
        } else {
            answer.into_iter().map(ContentBlock::text).collect()
        };
        let result = if failed {
            CallToolResult::error(content)
        } else {
            CallToolResult::success(content)
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
    fn tools_are_relayed_as_socket_lines_and_answers_become_results() {
        let (sender, source) = channel::channel::<Request>();
        let server = start(sender).unwrap();
        let url = format!("{}?workspace=7", server.url);

        let mut event_loop: EventLoop<Vec<(Option<u32>, String)>> = EventLoop::try_new().unwrap();
        event_loop
            .handle()
            .insert_source(source, |event, _, seen| {
                if let Event::Msg(request) = event {
                    let Call::Line(line) = request.call else {
                        return;
                    };
                    seen.push((request.workspace, line.clone()));
                    let reply = if line.contains("\"panes\"") {
                        vec![r#"{"panes":[{"name":"code","client":"decade-code"}]}"#.to_string()]
                    } else if line.contains("\"terminal\"") {
                        vec![r#"{"error":{"reason":"no client occupies pane terminal","message":{}}}"#.to_string()]
                    } else {
                        Vec::new()
                    };
                    let _ = request.reply.send(reply);
                }
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
                (names, show, panes, focus)
            })
        });

        let mut seen = Vec::new();
        while !client.is_finished() {
            event_loop
                .dispatch(Some(Duration::from_millis(50)), &mut seen)
                .unwrap();
        }
        let (names, show, panes, focus) = client.join().unwrap();

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
        assert_eq!(seen.len(), 3);
        assert!(seen.iter().all(|(workspace, _)| *workspace == Some(7)));
        assert_eq!(
            serde_json::from_str::<Value>(&seen[0].1).unwrap(),
            json!({"to": "code", "show": {"path": "src/main.rs", "line": 3}})
        );
        assert_eq!(show.is_error, Some(false));
        assert_eq!(
            show.content[0].as_text().unwrap().text,
            "sent code.show to decade"
        );
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
                .contains("no client occupies")
        );
    }

    #[test]
    fn scratch_open_defaults_to_a_terminal_and_refuses_arguments_that_are_not_strings() {
        let Ok(Call::ScratchOpen(launch)) =
            call_for("scratch.open", schema(json!({"program": "decade-code"})))
        else {
            panic!("scratch.open with a program is a launch");
        };
        assert_eq!(launch.program, "decade-code");
        assert!(launch.args.is_empty());
        assert!(launch.terminal);
        let args = schema(json!({"program": "decade-code", "args": [1]}));
        assert!(call_for("scratch.open", args).is_err());
        let payload = schema(json!({"verb": "show", "payload": "x"}));
        assert!(call_for("scratch.send", payload).is_err());
    }
}
