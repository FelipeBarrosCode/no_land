use std::net::SocketAddr;

use clap::Parser;

use crate::classifier::thresholds::ClassifierThresholds;

#[derive(Debug, Clone, Parser)]
#[command(name = "noland-network-agent", version, about)]
pub struct Config {
    #[arg(long, env = "NOLAND_UDP_ADDR", default_value = "127.0.0.1:6201")]
    pub udp_addr: SocketAddr,

    #[arg(long, env = "NOLAND_WS_ADDR", default_value = "127.0.0.1:6202")]
    pub ws_addr: SocketAddr,

    #[arg(long, env = "NOLAND_MAX_SESSIONS", default_value_t = 128)]
    pub max_sessions: usize,

    #[arg(long, env = "NOLAND_UDP_RATE_LIMIT", default_value_t = 20)]
    pub udp_rate_limit: usize,

    #[arg(long, env = "NOLAND_LOSS_GREAT", default_value_t = 0.1)]
    loss_great: f64,
    #[arg(long, env = "NOLAND_LOSS_GOOD", default_value_t = 0.5)]
    loss_good: f64,
    #[arg(long, env = "NOLAND_LOSS_POOR", default_value_t = 2.0)]
    loss_poor: f64,

    #[arg(long, env = "NOLAND_JITTER_GREAT_MS", default_value_t = 3.0)]
    jitter_great_ms: f64,
    #[arg(long, env = "NOLAND_JITTER_GOOD_MS", default_value_t = 7.0)]
    jitter_good_ms: f64,
    #[arg(long, env = "NOLAND_JITTER_POOR_MS", default_value_t = 15.0)]
    jitter_poor_ms: f64,

    #[arg(long, env = "NOLAND_SPREAD_GREAT_MS", default_value_t = 10.0)]
    spread_great_ms: f64,
    #[arg(long, env = "NOLAND_SPREAD_GOOD_MS", default_value_t = 20.0)]
    spread_good_ms: f64,
    #[arg(long, env = "NOLAND_SPREAD_POOR_MS", default_value_t = 40.0)]
    spread_poor_ms: f64,

    #[arg(long, env = "NOLAND_SPIKES_GREAT", default_value_t = 1.0)]
    spikes_great: f64,
    #[arg(long, env = "NOLAND_SPIKES_GOOD", default_value_t = 3.0)]
    spikes_good: f64,
    #[arg(long, env = "NOLAND_SPIKES_POOR", default_value_t = 10.0)]
    spikes_poor: f64,

    #[arg(long, env = "NOLAND_LATENCY_GREAT_MS", default_value_t = 30.0)]
    latency_great_ms: f64,
    #[arg(long, env = "NOLAND_LATENCY_GOOD_MS", default_value_t = 60.0)]
    latency_good_ms: f64,
    #[arg(long, env = "NOLAND_LATENCY_POOR_MS", default_value_t = 100.0)]
    latency_poor_ms: f64,
}

impl Config {
    pub fn classifier_thresholds(&self) -> ClassifierThresholds {
        ClassifierThresholds {
            loss_percent: [self.loss_great, self.loss_good, self.loss_poor],
            jitter_ms: [
                self.jitter_great_ms,
                self.jitter_good_ms,
                self.jitter_poor_ms,
            ],
            p95_median_spread_ms: [
                self.spread_great_ms,
                self.spread_good_ms,
                self.spread_poor_ms,
            ],
            spike_percent: [self.spikes_great, self.spikes_good, self.spikes_poor],
            latency_median_ms: [
                self.latency_great_ms,
                self.latency_good_ms,
                self.latency_poor_ms,
            ],
            ..ClassifierThresholds::default()
        }
    }
}
