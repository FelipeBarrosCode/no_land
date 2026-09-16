use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{
    net::{TcpListener, TcpStream},
    time::{interval, MissedTickBehavior},
};
use tokio_tungstenite::{accept_async, tungstenite::Message, WebSocketStream};
use uuid::Uuid;

use crate::{
    classifier::{classify, thresholds::ClassifierThresholds, Classification, ReasonCode},
    telemetry::{
        metrics::{snapshot, MetricsSnapshot},
        sample::MeasurementSample,
        state::StatusTransition,
    },
    SharedState,
};

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Auth {
        #[serde(rename = "sessionId")]
        session_id: Uuid,
        token: String,
    },
    MeasurementReport {
        #[serde(rename = "sessionId")]
        session_id: Uuid,
        samples: Vec<MeasurementSample>,
    },
    GetState,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StateView {
    published_status: crate::classifier::Quality,
    bad_since: Option<u64>,
    alert_eligible: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatsUpdate {
    #[serde(rename = "type")]
    message_type: &'static str,
    session_id: Uuid,
    generated_at_ms: u64,
    runtime_seconds: u64,
    metrics: MetricsSnapshot,
    classification: Classification,
    state: StateView,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KeyMetrics {
    median_rtt_ms: Option<f64>,
    p95_rtt_ms: Option<f64>,
    jitter_ms: f64,
    loss_percent: f64,
    spike_percent: f64,
    longest_loss_burst: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusChanged {
    #[serde(rename = "type")]
    message_type: &'static str,
    session_id: Uuid,
    previous: crate::classifier::Quality,
    current: crate::classifier::Quality,
    reasons: Vec<ReasonCode>,
    key_metrics: KeyMetrics,
    alert_eligible: bool,
}

struct Evaluation {
    stats: StatsUpdate,
    transition: Option<StatusChanged>,
}

pub async fn run(
    addr: SocketAddr,
    shared: SharedState,
    thresholds: Arc<ClassifierThresholds>,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    loop {
        let (stream, _) = listener.accept().await?;
        let shared = shared.clone();
        let thresholds = thresholds.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, shared, thresholds).await {
                eprintln!("WebSocket connection closed: {error:#}");
            }
        });
    }
}

async fn handle_connection(
    stream: TcpStream,
    shared: SharedState,
    thresholds: Arc<ClassifierThresholds>,
) -> anyhow::Result<()> {
    let mut websocket = accept_async(stream)
        .await
        .context("WebSocket handshake failed")?;
    let first = websocket
        .next()
        .await
        .ok_or_else(|| anyhow!("connection closed before auth"))??;
    let ClientMessage::Auth { session_id, token } = parse_client_message(first)? else {
        send_error(
            &mut websocket,
            "AUTH_REQUIRED",
            "first message must be auth",
        )
        .await?;
        return Ok(());
    };
    let token = decode_token(&token).ok_or_else(|| anyhow!("invalid auth token"));
    let token = match token {
        Ok(token) => token,
        Err(error) => {
            send_error(
                &mut websocket,
                "INVALID_TOKEN",
                "token must be exactly 64 hexadecimal characters",
            )
            .await?;
            return Err(error);
        }
    };

    {
        let mut registry = shared.lock().await;
        registry.register(session_id, token, Instant::now());
    }
    send_evaluation(
        &mut websocket,
        evaluate(&shared, session_id, &token, &thresholds).await?,
    )
    .await?;

    let mut ticker = interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ticker.tick().await;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let evaluation = match evaluate(&shared, session_id, &token, &thresholds).await {
                    Ok(evaluation) => evaluation,
                    Err(_) => {
                        send_error(&mut websocket, "SESSION_REPLACED", "session is no longer registered with this connection").await?;
                        return Ok(());
                    }
                };
                send_evaluation(&mut websocket, evaluation).await?;
            }
            incoming = websocket.next() => {
                let Some(incoming) = incoming else { return Ok(()); };
                let message = match parse_client_message(incoming?) {
                    Ok(message) => message,
                    Err(error) => {
                        send_error(&mut websocket, "INVALID_MESSAGE", &error.to_string()).await?;
                        continue;
                    }
                };
                match message {
                    ClientMessage::Auth { .. } => {
                        send_error(&mut websocket, "ALREADY_AUTHENTICATED", "auth is only accepted as the first message").await?;
                    }
                    ClientMessage::GetState => {
                        let evaluation = evaluate(&shared, session_id, &token, &thresholds).await?;
                        send_evaluation(&mut websocket, evaluation).await?;
                    }
                    ClientMessage::MeasurementReport { session_id: report_session, samples } => {
                        if report_session != session_id {
                            send_error(&mut websocket, "WRONG_SESSION", "report sessionId does not match authenticated session").await?;
                            continue;
                        }
                        if samples.len() > 100 {
                            send_error(&mut websocket, "TOO_MANY_SAMPLES", "reports may contain at most 100 samples").await?;
                            continue;
                        }
                        if !samples.iter().all(MeasurementSample::is_valid) {
                            send_error(&mut websocket, "INVALID_SAMPLE", "successful samples require a non-negative rttMs and lost samples must omit it").await?;
                            continue;
                        }
                        {
                            let mut registry = shared.lock().await;
                            let Some(session) = registry.get_mut(&session_id) else {
                                send_error(&mut websocket, "UNKNOWN_SESSION", "session is not registered").await?;
                                return Ok(());
                            };
                            if session.token != token {
                                send_error(&mut websocket, "SESSION_REPLACED", "session is no longer registered with this connection").await?;
                                return Ok(());
                            }
                            session.window.extend(samples, Instant::now());
                        }
                        let evaluation = evaluate(&shared, session_id, &token, &thresholds).await?;
                        send_evaluation(&mut websocket, evaluation).await?;
                    }
                }
            }
        }
    }
}

fn parse_client_message(message: Message) -> anyhow::Result<ClientMessage> {
    match message {
        Message::Text(text) => serde_json::from_str(text.as_ref()).context("invalid JSON message"),
        _ => Err(anyhow!("expected a text JSON message")),
    }
}

async fn evaluate(
    shared: &SharedState,
    session_id: Uuid,
    token: &[u8; 32],
    thresholds: &ClassifierThresholds,
) -> anyhow::Result<Evaluation> {
    let now = Instant::now();
    let generated_at_ms = unix_time_ms();
    let mut registry = shared.lock().await;
    let session = registry
        .get_mut(&session_id)
        .ok_or_else(|| anyhow!("session not registered"))?;
    if &session.token != token {
        return Err(anyhow!("session replaced"));
    }

    session.window.prune(now);
    let runtime_seconds = session.runtime(now).as_secs();
    let metrics = snapshot(&session.window, now);
    let classification = classify(&session.window, now, thresholds);
    let transition = session.state.evaluate(&classification, now);
    let state = StateView {
        published_status: session.state.published(),
        bad_since: session.state.bad_since().map(|bad_since| {
            generated_at_ms
                .saturating_sub(now.saturating_duration_since(bad_since).as_millis() as u64)
        }),
        alert_eligible: session.state.alert_eligible(),
    };
    let transition = transition.map(|transition| status_changed(session_id, transition, &metrics));

    Ok(Evaluation {
        stats: StatsUpdate {
            message_type: "stats_update",
            session_id,
            generated_at_ms,
            runtime_seconds,
            metrics,
            classification,
            state,
        },
        transition,
    })
}

fn status_changed(
    session_id: Uuid,
    transition: StatusTransition,
    metrics: &MetricsSnapshot,
) -> StatusChanged {
    let window = &metrics.last_60_seconds;
    StatusChanged {
        message_type: "status_changed",
        session_id,
        previous: transition.previous,
        current: transition.current,
        reasons: transition.reasons,
        key_metrics: KeyMetrics {
            median_rtt_ms: window.median_rtt_ms,
            p95_rtt_ms: window.p95_rtt_ms,
            jitter_ms: window.jitter_ms,
            loss_percent: window.loss_percent,
            spike_percent: window.spike_percent,
            longest_loss_burst: window.longest_loss_burst,
        },
        alert_eligible: transition.alert_eligible,
    }
}

async fn send_evaluation(
    websocket: &mut WebSocketStream<TcpStream>,
    evaluation: Evaluation,
) -> anyhow::Result<()> {
    websocket
        .send(Message::Text(serde_json::to_string(&evaluation.stats)?))
        .await?;
    if let Some(transition) = evaluation.transition {
        websocket
            .send(Message::Text(serde_json::to_string(&transition)?))
            .await?;
    }
    Ok(())
}

async fn send_error(
    websocket: &mut WebSocketStream<TcpStream>,
    code: &str,
    message: &str,
) -> anyhow::Result<()> {
    websocket
        .send(Message::Text(
            json!({"type": "error", "code": code, "message": message}).to_string(),
        ))
        .await?;
    Ok(())
}

fn decode_token(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let decoded = hex::decode(value).ok()?;
    decoded.try_into().ok()
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::decode_token;

    #[test]
    fn token_requires_exact_hex_length_and_accepts_uppercase() {
        assert_eq!(decode_token(&"AA".repeat(32)), Some([0xaa; 32]));
        assert!(decode_token(&"aa".repeat(31)).is_none());
        assert!(decode_token(&"gg".repeat(32)).is_none());
    }
}
