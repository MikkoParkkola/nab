//! WebSocket Client with TLS
//!
//! Features:
//! - Secure WebSocket (wss://) with TLS 1.3
//! - Automatic ping/pong for keep-alive
//! - Message fragmentation handling
//! - Browser fingerprint matching

use std::time::Duration;

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::{
        handshake::client::generate_key,
        http::{Request, Uri},
        Message,
    },
    Connector, MaybeTlsStream, WebSocketStream,
};
use tracing::{debug, info};

use crate::fingerprint::BrowserProfile;

/// WebSocket connection with TLS support and automatic ping/pong.
///
/// Wraps a `tokio-tungstenite` stream with browser-like headers and
/// transparent keep-alive handling. Ping frames received from the
/// server are automatically answered with Pong.
pub struct WebSocket {
    stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
    url: String,
}

impl WebSocket {
    /// Connect to a WebSocket endpoint
    pub async fn connect(url: &str, profile: &BrowserProfile) -> Result<Self> {
        // Ensure crypto provider is installed
        let _ = rustls::crypto::ring::default_provider().install_default();

        let uri: Uri = url.parse().context("Invalid WebSocket URL")?;
        let host = uri.host().context("No host in URL")?;

        // Build WebSocket upgrade request with browser headers
        let ws_key = generate_key();

        let request = Request::builder()
            .method("GET")
            .uri(url)
            .header("Host", host)
            .header("User-Agent", &profile.user_agent)
            .header("Accept-Language", &profile.accept_language)
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", &ws_key)
            .header("Origin", format!("https://{host}"))
            .body(())
            .context("Failed to build WebSocket request")?;

        info!("Connecting WebSocket to {}", url);

        // Connect with TLS using rustls
        let connector = Connector::Rustls(std::sync::Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates({
                    let mut roots = rustls::RootCertStore::empty();
                    let certs = rustls_native_certs::load_native_certs();
                    for cert in certs.certs {
                        let _ = roots.add(cert);
                    }
                    roots
                })
                .with_no_client_auth(),
        ));

        let (stream, response) =
            connect_async_tls_with_config(request, None, false, Some(connector))
                .await
                .context("WebSocket connection failed")?;

        debug!("WebSocket connected: {:?}", response.status());

        Ok(Self {
            stream,
            url: url.to_string(),
        })
    }

    /// Send a text message
    pub async fn send_text(&mut self, text: &str) -> Result<()> {
        self.stream
            .send(Message::Text(text.to_string()))
            .await
            .context("Failed to send text message")?;
        debug!("Sent text: {} bytes", text.len());
        Ok(())
    }

    /// Send a binary message.
    pub async fn send_binary(&mut self, data: Vec<u8>) -> Result<()> {
        let len = data.len();
        self.stream
            .send(Message::Binary(data))
            .await
            .context("Failed to send binary message")?;
        debug!("Sent binary: {} bytes", len);
        Ok(())
    }

    /// Send a ping
    pub async fn ping(&mut self) -> Result<()> {
        self.stream
            .send(Message::Ping(vec![]))
            .await
            .context("Failed to send ping")?;
        Ok(())
    }

    /// Receive the next application-level message.
    ///
    /// Ping frames are answered automatically with Pong. Pong and raw
    /// Frame messages are silently consumed so the caller only sees
    /// Text, Binary, or Close messages.
    pub async fn recv(&mut self) -> Result<Option<WebSocketMessage>> {
        loop {
            match self.stream.next().await {
                Some(Ok(msg)) => match msg {
                    Message::Text(text) => return Ok(Some(WebSocketMessage::Text(text))),
                    Message::Binary(data) => return Ok(Some(WebSocketMessage::Binary(data))),
                    Message::Ping(data) => {
                        // Auto-respond with pong
                        let _ = self.stream.send(Message::Pong(data)).await;
                        continue;
                    }
                    Message::Pong(_) => {
                        debug!("Received pong");
                        continue;
                    }
                    Message::Close(frame) => {
                        info!("WebSocket closed: {:?}", frame);
                        return Ok(Some(WebSocketMessage::Close));
                    }
                    Message::Frame(_) => continue,
                },
                Some(Err(e)) => {
                    return Err(anyhow::Error::new(e).context("WebSocket receive failed"))
                }
                None => return Ok(None),
            }
        }
    }

    /// Receive with timeout
    pub async fn recv_timeout(&mut self, timeout: Duration) -> Result<Option<WebSocketMessage>> {
        match tokio::time::timeout(timeout, self.recv()).await {
            Ok(result) => result,
            Err(_) => Ok(None),
        }
    }

    /// Close the connection
    pub async fn close(&mut self) -> Result<()> {
        self.stream
            .close(None)
            .await
            .context("Failed to close WebSocket")?;
        info!("WebSocket closed");
        Ok(())
    }

    /// Get the URL
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }
}

/// Application-level WebSocket message types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebSocketMessage {
    /// UTF-8 text frame.
    Text(String),
    /// Binary data frame.
    Binary(Vec<u8>),
    /// Connection close frame.
    Close,
}

impl WebSocketMessage {
    /// Returns `true` if this is a [`Text`](WebSocketMessage::Text) message.
    #[must_use]
    pub fn is_text(&self) -> bool {
        matches!(self, Self::Text(_))
    }

    /// Returns `true` if this is a [`Binary`](WebSocketMessage::Binary) message.
    #[must_use]
    pub fn is_binary(&self) -> bool {
        matches!(self, Self::Binary(_))
    }

    /// Returns `true` if this is a [`Close`](WebSocketMessage::Close) message.
    #[must_use]
    pub fn is_close(&self) -> bool {
        matches!(self, Self::Close)
    }

    /// Borrow the text payload, if this is a text message.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Borrow the binary payload, if this is a binary message.
    #[must_use]
    pub fn as_binary(&self) -> Option<&[u8]> {
        match self {
            Self::Binary(b) => Some(b),
            _ => None,
        }
    }
}

/// JSON-RPC 2.0 client layered on top of a [`WebSocket`].
///
/// Assigns auto-incrementing request IDs and correlates responses by ID
/// within a caller-supplied timeout.
pub struct JsonRpcWebSocket {
    ws: WebSocket,
    request_id: u64,
}

impl JsonRpcWebSocket {
    /// Connect to a JSON-RPC WebSocket endpoint.
    pub async fn connect(url: &str, profile: &BrowserProfile) -> Result<Self> {
        let ws = WebSocket::connect(url, profile)
            .await
            .context("JSON-RPC WebSocket connection failed")?;
        Ok(Self { ws, request_id: 0 })
    }

    /// Send a JSON-RPC 2.0 request and wait for the matching response.
    ///
    /// Returns the deserialized `result` field on success or an error if
    /// the response contains an `error` field or the timeout expires.
    pub async fn call<P: serde::Serialize, R: serde::de::DeserializeOwned>(
        &mut self,
        method: &str,
        params: P,
        timeout: Duration,
    ) -> Result<R> {
        self.request_id += 1;
        let id = self.request_id;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });

        self.ws
            .send_text(&request.to_string())
            .await
            .with_context(|| format!("Failed to send JSON-RPC request for method '{method}'"))?;

        // Wait for matching response
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            if let Some(WebSocketMessage::Text(text)) =
                self.ws.recv_timeout(Duration::from_millis(100)).await?
            {
                let response: serde_json::Value =
                    serde_json::from_str(&text).context("Invalid JSON in JSON-RPC response")?;
                if response.get("id") == Some(&serde_json::json!(id)) {
                    if let Some(error) = response.get("error") {
                        return Err(anyhow::anyhow!("JSON-RPC error: {error}"));
                    }
                    if let Some(result) = response.get("result") {
                        return serde_json::from_value(result.clone())
                            .context("Failed to deserialize JSON-RPC result");
                    }
                }
            }
        }

        Err(anyhow::anyhow!(
            "Timeout ({timeout:?}) waiting for JSON-RPC response to method '{method}'"
        ))
    }

    /// Close the underlying WebSocket connection.
    pub async fn close(&mut self) -> Result<()> {
        self.ws.close().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::chrome_profile;

    // -- WebSocketMessage unit tests (no network) --

    #[test]
    fn test_message_text_accessors() {
        let msg = WebSocketMessage::Text("hello".to_string());
        assert!(msg.is_text());
        assert!(!msg.is_binary());
        assert!(!msg.is_close());
        assert_eq!(msg.as_text(), Some("hello"));
        assert_eq!(msg.as_binary(), None);
    }

    #[test]
    fn test_message_binary_accessors() {
        let msg = WebSocketMessage::Binary(vec![1, 2, 3]);
        assert!(!msg.is_text());
        assert!(msg.is_binary());
        assert!(!msg.is_close());
        assert_eq!(msg.as_text(), None);
        assert_eq!(msg.as_binary(), Some(&[1u8, 2, 3][..]));
    }

    #[test]
    fn test_message_close_accessors() {
        let msg = WebSocketMessage::Close;
        assert!(!msg.is_text());
        assert!(!msg.is_binary());
        assert!(msg.is_close());
        assert_eq!(msg.as_text(), None);
        assert_eq!(msg.as_binary(), None);
    }

    #[test]
    fn test_message_equality() {
        assert_eq!(
            WebSocketMessage::Text("a".into()),
            WebSocketMessage::Text("a".into())
        );
        assert_ne!(
            WebSocketMessage::Text("a".into()),
            WebSocketMessage::Text("b".into())
        );
        assert_ne!(
            WebSocketMessage::Text("a".into()),
            WebSocketMessage::Binary(b"a".to_vec())
        );
        assert_eq!(WebSocketMessage::Close, WebSocketMessage::Close);
    }

    #[test]
    fn test_message_clone() {
        let msg = WebSocketMessage::Binary(vec![42; 100]);
        let cloned = msg.clone();
        assert_eq!(msg, cloned);
    }

    #[test]
    fn test_message_debug() {
        let msg = WebSocketMessage::Text("test".to_string());
        let debug = format!("{msg:?}");
        assert!(debug.contains("Text"));
        assert!(debug.contains("test"));
    }

    // -- Integration test (network-dependent) --

    #[tokio::test]
    async fn test_websocket_echo() {
        // Install crypto provider for rustls
        let _ = rustls::crypto::ring::default_provider().install_default();

        // Use a public echo server
        let profile = chrome_profile();
        let result = WebSocket::connect("wss://echo.websocket.org", &profile).await;

        // This test may fail if the echo server is down
        if let Ok(mut ws) = result {
            ws.send_text("Hello, WebSocket!").await.unwrap();
            if let Ok(Some(msg)) = ws.recv_timeout(Duration::from_secs(5)).await {
                assert!(msg.is_text());
                println!("Echo: {:?}", msg.as_text());
            }
            let _ = ws.close().await;
        } else {
            println!("Echo server unavailable, skipping test");
        }
    }
}
