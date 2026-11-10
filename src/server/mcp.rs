use std::io;
use std::net::TcpListener;

use decade_channel::{Clear, Highlight, Show};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use smithay::reexports::calloop::channel::Sender;
use tokio::sync::oneshot;

use crate::server::scratch::Launch;

pub struct Request {
    /// The workspace named in the address the call came in on.
    pub workspace: u32,
    pub call: Call,

    /// The tool result's text, or the reason the call failed.
    pub reply: oneshot::Sender<Result<String, String>>,
}

pub enum Call {
    /// A `show`, `highlight` or `clear` of `kind`, for pane `pane` or
    /// else the one pane whose program shows `kind`.
    Kind {
        verb: &'static str,
        kind: String,
        pane: Option<String>,
        payload: Value,
    },

    /// A pane verb for the program in the scratch pane.
    ScratchSend {
        verb: String,
        payload: Value,
    },

    /// Point at pane `pane`, with a note beside it.
    Point {
        pane: String,
        note: Option<String>,
    },
    Panes,
    Agents,
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
    let kind = json!({"type": "string", "description": "a kind of thing to show, as panes lists them, such as file"});
    let target = json!({"type": "string", "description": "what to show, in the form the kind takes: a path for file"});
    let pane = json!({"type": "string", "description": "the pane to use, needed only when several panes show the kind"});
    vec![
        Tool::new(
            "show",
            "Show a target in the pane whose program shows its kind",
            schema(json!({
                "type": "object",
                "properties": {
                    "kind": kind,
                    "target": target,
                    "at": {"description": "where in the target, in the form the kind takes: a 1-based line number for file"},
                    "pane": pane
                },
                "required": ["kind", "target"]
            })),
        ),
        Tool::new(
            "highlight",
            "Highlight parts of a target in the pane whose program shows its kind, replacing the highlights there",
            schema(json!({
                "type": "object",
                "properties": {
                    "kind": kind,
                    "target": target,
                    "ranges": {"description": "the parts to highlight, in the form the kind takes: inclusive 1-based [from, to] line pairs for file"},
                    "pane": pane
                },
                "required": ["kind", "target", "ranges"]
            })),
        ),
        Tool::new(
            "clear",
            "Remove every highlight of a kind",
            schema(json!({
                "type": "object",
                "properties": {"kind": kind, "pane": pane},
                "required": ["kind"]
            })),
        ),
        Tool::new(
            "panes",
            "List the panes in this workspace, the program occupying each and the kinds it shows. client is the name a program gave when it connected to decade. It is null when no program in the pane has connected, though a terminal or another window may still be there. Only programs written for decade show kinds, so a terminal's shows is empty",
            schema(json!({"type": "object", "properties": {}})),
        ),
        Tool::new(
            "agents",
            "List the agents in this workspace: each one's name, pane, state, seconds in that state, question, activity, model, context use, tokens, cost and whether it is connected",
            schema(json!({"type": "object", "properties": {}})),
        ),
        Tool::new(
            "point",
            "Draw the user's eye to a pane without moving keyboard focus: its outline pulses for up to 30 seconds, until the user focuses it or the next point",
            schema(json!({
                "type": "object",
                "properties": {
                    "pane": {"type": "string", "description": "a pane name, as panes lists it"},
                    "note": {"type": "string", "description": "a few words shown on the pane's outline"}
                },
                "required": ["pane"]
            })),
        ),
        Tool::new(
            "scratch.open",
            "Start a program in the scratch pane, closing the program there first. The program runs in the workspace's directory with decade's environment. With terminal false, decade puts the first window of the program, or of a process under it, in the scratch pane",
            schema(json!({
                "type": "object",
                "properties": {
                    "program": {"type": "string", "description": "a program on decade's PATH, or a path relative to the workspace's directory"},
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

/// The arguments of `point`.
#[derive(Deserialize)]
struct Point {
    pane: String,
    note: Option<String>,
}

/// The arguments of `show`, `highlight` and `clear`: the verb's payload
/// and an optional pane.
#[derive(Deserialize)]
struct ToKind<T> {
    pane: Option<String>,
    #[serde(flatten)]
    payload: T,
}

/// The arguments of `scratch.send`.
#[derive(Deserialize)]
struct ScratchSend {
    verb: String,
    payload: Map<String, Value>,
}

fn call_for(name: &str, arguments: Map<String, Value>) -> Result<Call, String> {
    match name {
        "show" => {
            let ToKind { pane, payload } = parse::<ToKind<Show>>(name, arguments)?;
            Ok(kind_call("show", pane, payload.kind.clone(), payload))
        }
        "highlight" => {
            let ToKind { pane, payload } = parse::<ToKind<Highlight>>(name, arguments)?;
            Ok(kind_call("highlight", pane, payload.kind.clone(), payload))
        }
        "clear" => {
            let ToKind { pane, payload } = parse::<ToKind<Clear>>(name, arguments)?;
            Ok(kind_call("clear", pane, payload.kind.clone(), payload))
        }
        "panes" => Ok(Call::Panes),
        "agents" => Ok(Call::Agents),
        "point" => {
            let Point { pane, note } = parse(name, arguments)?;
            Ok(Call::Point { pane, note })
        }
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

fn kind_call(
    verb: &'static str,
    pane: Option<String>,
    kind: String,
    payload: impl Serialize,
) -> Call {
    Call::Kind {
        verb,
        kind,
        pane,
        payload: serde_json::to_value(payload).expect("a payload of strings and JSON values"),
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
                 workspace this agent was started in, and panes lists them with the kinds of \
                 thing the program in each can show, such as file. show brings a target of a \
                 kind into view, highlight marks parts of it, and clear removes the marks. Each \
                 goes to the pane whose program shows that kind; name the pane when several \
                 do. For file the target is a path, at is a line and ranges are [from, to] \
                 line pairs. The user may mark one pane as the scratch pane, where agents \
                 show things. scratch.open starts a program in it, \
                 replacing the program there, and scratch.send sends that program a message. \
                 Both fail when the workspace has no scratch pane. point draws the user's eye \
                 to a pane without taking keyboard focus. agents lists the agents in the \
                 workspace with what each last reported.",
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
                    Call::Kind {
                        verb,
                        kind,
                        pane,
                        payload,
                    } => {
                        let call = json!({"kind": kind, "pane": pane, verb: payload});
                        seen.push((request.workspace, call));
                        Ok("sent".to_string())
                    }
                    Call::Panes => {
                        Ok(r#"{"panes":[{"name":"code","client":"decade-code"}]}"#.to_string())
                    }
                    Call::Point { pane, .. } => Err(format!("no pane named {pane}")),
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
                    .call_tool(CallToolRequestParams::new("show").with_arguments(schema(
                        json!({"kind": "file", "target": "src/main.rs", "at": 3}),
                    )))
                    .await
                    .unwrap();
                let panes = client
                    .call_tool(CallToolRequestParams::new("panes"))
                    .await
                    .unwrap();
                let point = client
                    .call_tool(
                        CallToolRequestParams::new("point")
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
                (names, show, panes, point, refused)
            })
        });

        let mut seen = Vec::new();
        while !client.is_finished() {
            event_loop
                .dispatch(Some(Duration::from_millis(50)), &mut seen)
                .unwrap();
        }
        let (names, show, panes, point, refused) = client.join().unwrap();

        assert_eq!(
            names,
            [
                "show",
                "highlight",
                "clear",
                "panes",
                "agents",
                "point",
                "scratch.open",
                "scratch.send"
            ]
        );
        assert_eq!(
            seen,
            [(
                7,
                json!({
                    "kind": "file",
                    "pane": null,
                    "show": {"kind": "file", "target": "src/main.rs", "at": 3}
                })
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
        assert_eq!(point.is_error, Some(true));
        assert!(
            point.content[0]
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
        assert!(matches!(call_for("agents", Map::new()), Ok(Call::Agents)));
        let missing = call_for("point", Map::new());
        assert_eq!(missing.err().unwrap(), "point: missing field `pane`");
        assert_eq!(
            call_for("focus", Map::new()).err().unwrap(),
            "no tool named focus"
        );
    }

    #[test]
    fn point_takes_a_pane_and_an_optional_note() {
        let Ok(Call::Point { pane, note }) = call_for("point", schema(json!({"pane": "code"})))
        else {
            panic!("point with a pane is a point");
        };
        assert_eq!((pane.as_str(), note), ("code", None));
        let noted = schema(json!({"pane": "code", "note": "the failing test"}));
        let Ok(Call::Point { note, .. }) = call_for("point", noted) else {
            panic!("point with a note is a point");
        };
        assert_eq!(note.as_deref(), Some("the failing test"));
        let Err(reason) = call_for("point", schema(json!({"pane": "code", "note": 3}))) else {
            panic!("a number is not a note");
        };
        assert!(reason.starts_with("point: invalid type: integer"));
    }

    #[test]
    fn show_highlight_and_clear_take_a_payload_and_an_optional_pane() {
        let show = json!({"kind": "file", "target": "a.rs", "at": 7, "pane": "code"});
        let Ok(Call::Kind {
            verb,
            kind,
            pane,
            payload,
        }) = call_for("show", schema(show))
        else {
            panic!("show with a kind and target is a kind call");
        };
        assert_eq!(
            (verb, kind.as_str(), pane.as_deref()),
            ("show", "file", Some("code"))
        );
        assert_eq!(payload, json!({"kind": "file", "target": "a.rs", "at": 7}));
        let highlight = json!({"kind": "file", "target": "a.rs", "ranges": [[1, 3]]});
        let Ok(Call::Kind {
            verb,
            pane,
            payload,
            ..
        }) = call_for("highlight", schema(highlight))
        else {
            panic!("highlight with ranges is a kind call");
        };
        assert_eq!((verb, pane), ("highlight", None));
        assert_eq!(payload["ranges"], json!([[1, 3]]));
        let Ok(Call::Kind { verb, payload, .. }) =
            call_for("clear", schema(json!({"kind": "url"})))
        else {
            panic!("clear with a kind is a kind call");
        };
        assert_eq!((verb, payload), ("clear", json!({"kind": "url"})));
        let missing = call_for(
            "highlight",
            schema(json!({"kind": "file", "target": "a.rs"})),
        );
        assert_eq!(missing.err().unwrap(), "highlight: missing field `ranges`");
        let Err(reason) = call_for("show", schema(json!({"kind": "file", "target": 3}))) else {
            panic!("a number is not a target");
        };
        assert!(reason.starts_with("show: invalid type: integer"));
    }
}
