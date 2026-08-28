//! Scripted WebSocket tests retained from the live-tested Responses client.

use futures::{SinkExt, StreamExt};
use serdes_ai_core::messages::{ModelRequest, ModelRequestPart, SystemPromptPart, UserPromptPart};
use serdes_ai_models::model::{Model, ModelRequestParameters};
use serdes_ai_responses::client::OpenResponsesModel;
use serdes_ai_responses::types::{
    CreateResponseRequest, OutputContent, OutputItem, ResponseObject, ResponseStatus,
    ResponseUsage, StreamEvent,
};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_async, WebSocketStream};

/// A user turn with a plain text prompt.
fn user_turn(text: &str) -> ModelRequest {
    ModelRequest::with_parts(vec![ModelRequestPart::UserPrompt(UserPromptPart::new(
        text,
    ))])
}

/// A system turn.
fn system_turn(text: &str) -> ModelRequest {
    ModelRequest::with_parts(vec![ModelRequestPart::SystemPrompt(SystemPromptPart::new(
        text,
    ))])
}

fn params() -> ModelRequestParameters {
    ModelRequestParameters::new()
}

fn settings() -> serdes_ai_core::ModelSettings {
    serdes_ai_core::ModelSettings::default()
}

/// Concatenated text parts of a response.
fn text_of(response: &serdes_ai_core::ModelResponse) -> String {
    response
        .text_parts()
        .map(|part| part.content.as_str())
        .collect()
}
type FakeWs = WebSocketStream<tokio::net::TcpStream>;

/// Accept one websocket connection.
async fn accept_ws(listener: &TcpListener) -> FakeWs {
    let (stream, _) = listener.accept().await.unwrap();
    accept_async(stream).await.unwrap()
}

/// Read one `response.create` frame from the client.
async fn read_turn(ws: &mut FakeWs) -> CreateResponseRequest {
    loop {
        let message = ws.next().await.expect("frame").expect("ws ok");
        match message {
            Message::Text(text) => {
                let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
                assert_eq!(value["type"], "response.create", "unexpected frame: {text}");
                // Codex frames are flat; everything except `type` is the
                // response payload.
                value.as_object_mut().expect("frame object").remove("type");
                return serde_json::from_value(value).unwrap();
            }
            Message::Close(_) => panic!("client closed before sending a turn"),
            _ => continue,
        }
    }
}

/// Run a minimal-but-realistic turn: item added, text delta, item done,
/// completed. The client assembles parts from the streamed item events, so
/// a bare `response.completed` would leave the folded response empty.
async fn send_completed_turn(ws: &mut FakeWs, id: &str, request: &CreateResponseRequest) {
    let mut response = ResponseObject::in_progress(id, 0, request.model.clone(), request);
    response.status = ResponseStatus::Completed;
    response.output = vec![OutputItem::Message {
        id: format!("msg_{id}"),
        role: "assistant".to_string(),
        status: serdes_ai_responses::types::OutputItemStatus::Completed,
        content: vec![OutputContent::OutputText {
            text: "ok".to_string(),
            annotations: Vec::new(),
        }],
    }];
    response.usage = Some(ResponseUsage {
        input_tokens: Some(1),
        output_tokens: Some(1),
        total_tokens: Some(2),
    });

    let events = vec![
        StreamEvent::OutputItemAdded {
            sequence_number: 1,
            output_index: 0,
            item: OutputItem::Message {
                id: format!("msg_{id}"),
                role: "assistant".to_string(),
                status: serdes_ai_responses::types::OutputItemStatus::InProgress,
                content: Vec::new(),
            },
        },
        StreamEvent::OutputTextDelta {
            sequence_number: 2,
            item_id: format!("msg_{id}"),
            output_index: 0,
            content_index: 0,
            delta: "ok".to_string(),
        },
        StreamEvent::OutputItemDone {
            sequence_number: 3,
            output_index: 0,
            item: response.output[0].clone(),
        },
        StreamEvent::ResponseCompleted {
            sequence_number: 4,
            response,
        },
    ];
    for event in &events {
        send_event(ws, event).await;
    }
}

async fn send_event(ws: &mut FakeWs, event: &StreamEvent) {
    ws.send(Message::Text(serde_json::to_string(event).unwrap()))
        .await
        .unwrap();
}

#[tokio::test]
async fn stale_continuation_clears_chain_and_replays_full_input() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_ws(&listener).await;

        // Turn 1: full input, no continuation.
        let request = read_turn(&mut ws).await;
        assert!(request.previous_response_id.is_none());
        send_completed_turn(&mut ws, "resp_1", &request).await;

        // Turn 2: client chains onto resp_1 and sends only the new item.
        let request = read_turn(&mut ws).await;
        assert_eq!(request.previous_response_id.as_deref(), Some("resp_1"));
        let items = match &request.input {
            serdes_ai_responses::types::ResponseInput::Items(items) => items.len(),
            other => panic!("expected items, got {other:?}"),
        };
        assert_eq!(items, 1, "chained turn sends only new input");

        // The chain is stale from the server's point of view.
        let envelope = serde_json::json!({
            "type": "error",
            "status_code": 404,
            "error": {
                "code": "previous_response_not_found",
                "message": "previous response not found: resp_1",
            }
        });
        ws.send(Message::Text(envelope.to_string())).await.unwrap();

        // Retry: no continuation id, full input replayed.
        let replay = read_turn(&mut ws).await;
        assert!(replay.previous_response_id.is_none());
        let items = match &replay.input {
            serdes_ai_responses::types::ResponseInput::Items(items) => items.len(),
            other => panic!("expected items, got {other:?}"),
        };
        assert!(
            items >= 2,
            "replay carries the full input, got {items} items"
        );
        send_completed_turn(&mut ws, "resp_2", &replay).await;
    });

    let client = OpenResponsesModel::new("test-model", format!("ws://{addr}/v1/responses"));
    let mut history = vec![system_turn("sys"), user_turn("first")];
    let first = client
        .request(&history, &settings(), &params())
        .await
        .expect("first turn");
    history.push(ModelRequest::with_parts(vec![
        ModelRequestPart::ModelResponse(Box::new(first)),
    ]));
    history.push(user_turn("second"));

    let second = client
        .request(&history, &settings(), &params())
        .await
        .expect("second turn after stale-continuation recovery");
    assert_eq!(text_of(&second), "ok");
    server.await.unwrap();
}

#[tokio::test]
async fn connection_limit_error_reconnects_on_a_fresh_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        // First connection: refuse the very first turn with the limit error
        // and drop the socket.
        let mut ws = accept_ws(&listener).await;
        let _request = read_turn(&mut ws).await;
        let envelope = serde_json::json!({
            "type": "error",
            "status_code": 429,
            "error": {
                "code": "websocket_connection_limit_reached",
                "message": "websocket connection lifetime limit reached",
            }
        });
        ws.send(Message::Text(envelope.to_string())).await.unwrap();
        ws.send(Message::Close(None)).await.unwrap();
        drop(ws);

        // Second connection: fresh session, full input, no continuation.
        let mut ws = accept_ws(&listener).await;
        let request = read_turn(&mut ws).await;
        assert!(request.previous_response_id.is_none());
        send_completed_turn(&mut ws, "resp_1", &request).await;
    });

    let client = OpenResponsesModel::new("test-model", format!("ws://{addr}/v1/responses"));
    let response = client
        .request(&[user_turn("hello")], &settings(), &params())
        .await
        .expect("turn succeeds after reconnect");
    assert_eq!(text_of(&response), "ok");
    server.await.unwrap();
}
