//! `ReqwestTransport` against a local HTTP server: what is sent, and how
//! failures are classified (design.md §19.2).

use std::time::Duration;

use postal_mcp::http::{
    HttpError, Method, Part, Request, ReqwestTransport, RetryPolicy, Retrying, Transport,
};
use serde_json::json;
use wiremock::matchers::{body_json, header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn json_post_carries_auth_idempotency_key_and_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/letters"))
        // Lob style: API key as the basic-auth username, empty password.
        .and(header("authorization", "Basic dGVzdF9rZXk6"))
        .and(header("idempotency-key", "op_123"))
        .and(header("lob-version", "2020-02-11"))
        .and(header("content-type", "application/json"))
        .and(body_json(json!({"to": "adr_1"})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": "ltr_1"}))
                .insert_header("x-request-id", "req_9"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let response = ReqwestTransport::new()
        .unwrap()
        .send(
            Request::new(Method::Post, format!("{}/v1/letters", server.uri()))
                .basic_auth("test_key", None)
                .idempotency_key("Idempotency-Key", "op_123")
                .header("Lob-Version", "2020-02-11")
                .json(json!({"to": "adr_1"})),
        )
        .await
        .unwrap();

    assert_eq!(response.status, 200);
    assert_eq!(response.header("X-Request-Id"), Some("req_9"));
    assert_eq!(response.json::<serde_json::Value>().unwrap()["id"], "ltr_1");
}

#[tokio::test]
async fn multipart_upload_sends_file_and_fields() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/faxes"))
        .and(header_exists("content-type"))
        .respond_with(|req: &wiremock::Request| {
            let ct = req
                .headers
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            let body = String::from_utf8_lossy(&req.body).into_owned();
            let ok = ct.starts_with("multipart/form-data; boundary=")
                && body.contains("name=\"to\"")
                && body.contains("+12025550199")
                && body.contains("filename=\"letter.pdf\"")
                && body.contains("Content-Type: application/pdf")
                && body.contains("%PDF-1.4");
            ResponseTemplate::new(if ok { 200 } else { 400 })
        })
        .mount(&server)
        .await;

    let response = ReqwestTransport::new()
        .unwrap()
        .send(
            Request::new(Method::Post, format!("{}/v2/faxes", server.uri())).multipart(vec![
                Part {
                    name: "to".into(),
                    filename: None,
                    content_type: None,
                    data: b"+12025550199".to_vec(),
                },
                Part {
                    name: "file".into(),
                    filename: Some("letter.pdf".into()),
                    content_type: Some("application/pdf".into()),
                    data: b"%PDF-1.4 test".to_vec(),
                },
            ]),
        )
        .await
        .unwrap();
    assert_eq!(response.status, 200);
}

#[tokio::test]
async fn error_status_is_a_response_not_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(422).set_body_json(json!({"error": {"message": "bad address"}})),
        )
        .mount(&server)
        .await;

    let response = ReqwestTransport::new()
        .unwrap()
        .send(Request::new(Method::Get, server.uri()))
        .await
        .unwrap();
    assert_eq!(response.status, 422);
    assert!(!response.is_success());
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(307).insert_header("location", "https://elsewhere.example/"),
        )
        .mount(&server)
        .await;

    let response = ReqwestTransport::new()
        .unwrap()
        .send(Request::new(Method::Post, server.uri()).json(json!({})))
        .await
        .unwrap();
    assert_eq!(response.status, 307);
}

#[tokio::test]
async fn connection_refused_is_not_sent() {
    // Bind a port, then free it, so nothing listens there.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();

    let err = ReqwestTransport::new()
        .unwrap()
        .send(Request::new(Method::Post, format!("http://127.0.0.1:{port}/")).json(json!({})))
        .await
        .unwrap_err();
    assert!(matches!(err, HttpError::NotSent(_)), "{err:?}");
    assert!(!err.provider_may_have_received());
}

#[tokio::test]
async fn timeout_after_sending_is_ambiguous() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
        .expect(1) // the provider did receive it
        .mount(&server)
        .await;

    let err = ReqwestTransport::new()
        .unwrap()
        .send(
            Request::new(Method::Post, server.uri())
                .json(json!({}))
                .timeout(Duration::from_millis(200)),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, HttpError::Ambiguous(_)), "{err:?}");
    assert!(err.provider_may_have_received());
}

#[tokio::test]
async fn oversized_body_is_ambiguous() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 4096]))
        .mount(&server)
        .await;

    let err = ReqwestTransport::new()
        .unwrap()
        .send(Request::new(Method::Get, server.uri()).max_response_bytes(1024))
        .await
        .unwrap_err();
    // The status survives: the provider's answer is known, only its body is not.
    let HttpError::ResponseTooLarge { head, limit } = &err else {
        panic!("{err:?}");
    };
    assert_eq!((head.status, *limit), (200, 1024));
    assert!(head.body.is_empty());
    assert!(err.provider_may_have_received());
}

#[tokio::test]
async fn error_messages_omit_the_url() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let err = ReqwestTransport::new()
        .unwrap()
        .send(Request::new(
            Method::Get,
            format!("http://127.0.0.1:{port}/?api_key=sk_secret"),
        ))
        .await
        .unwrap_err();
    assert!(!err.to_string().contains("sk_secret"), "{err}");
}

#[tokio::test]
async fn retrying_transport_honors_retry_after_for_keyed_post() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("x-dedupe-token", "op_7")) // the adapter names the header
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "ltr_7"})))
        .expect(1)
        .mount(&server)
        .await;

    let transport = Retrying::new(ReqwestTransport::new().unwrap(), RetryPolicy::default());
    let response = transport
        .send(
            Request::new(Method::Post, server.uri())
                .idempotency_key("X-Dedupe-Token", "op_7")
                .json(json!({})),
        )
        .await
        .unwrap();
    assert_eq!(response.status, 200);
}

#[tokio::test]
async fn retrying_transport_never_resends_after_a_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
        .expect(1) // exactly one delivery, despite the idempotency key
        .mount(&server)
        .await;

    let transport = Retrying::new(ReqwestTransport::new().unwrap(), RetryPolicy::default());
    let err = transport
        .send(
            Request::new(Method::Post, server.uri())
                .idempotency_key("Idempotency-Key", "op_8")
                .json(json!({}))
                .timeout(Duration::from_millis(200)),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, HttpError::Ambiguous(_)), "{err:?}");
}
