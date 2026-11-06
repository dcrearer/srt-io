//! Socket performance statistics.
//!
//! [`Stats`] is a safe, owned projection of libsrt's `SRT_TRACEBSTATS`
//! (`CBytePerfMon`) performance monitor, exposing the fields an application
//! typically surfaces on a dashboard: link quality (RTT, estimated bandwidth),
//! throughput, packet and byte counters, and live buffer levels. Obtain one
//! with [`Socket::stats`](crate::Socket::stats).

/// A snapshot of a socket's transport statistics.
///
/// The packet and byte counters are cumulative totals since the socket was
/// created (libsrt's `*Total` fields); rates and buffer levels are
/// instantaneous (this wrapper reads libsrt's instantaneous variant). All
/// fields are plain values — reading them never touches libsrt again.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct Stats {
    /// Smoothed round-trip time, in milliseconds (`msRTT`).
    pub rtt_ms: f64,
    /// Estimated link bandwidth, in Mbps (`mbpsBandwidth`).
    pub bandwidth_mbps: f64,
    /// Current sending rate, in Mbps (`mbpsSendRate`).
    pub send_rate_mbps: f64,
    /// Current receiving rate, in Mbps (`mbpsRecvRate`).
    pub recv_rate_mbps: f64,

    /// Total data packets sent (`pktSentTotal`).
    pub packets_sent: i64,
    /// Total data packets received (`pktRecvTotal`).
    pub packets_received: i64,
    /// Total packets reported lost on the receive side (`pktRcvLossTotal`).
    pub packets_lost: i64,
    /// Total packets retransmitted by the sender (`pktRetransTotal`).
    pub packets_retransmitted: i64,
    /// Total packets dropped by the sender as too-late (`pktSndDropTotal`).
    pub packets_sent_dropped: i64,
    /// Total packets dropped on the receive side as too-late (`pktRcvDropTotal`).
    pub packets_recv_dropped: i64,

    /// Total payload bytes sent (`byteSentTotal`).
    pub bytes_sent: u64,
    /// Total payload bytes received (`byteRecvTotal`).
    pub bytes_received: u64,
    /// Total payload bytes retransmitted (`byteRetransTotal`).
    pub bytes_retransmitted: u64,

    /// Packets currently buffered in the send buffer (`pktSndBuf`).
    pub send_buffer_packets: i32,
    /// Bytes currently buffered in the send buffer (`byteSndBuf`).
    pub send_buffer_bytes: i32,
    /// Packets currently buffered in the receive buffer (`pktRcvBuf`).
    pub recv_buffer_packets: i32,
    /// Bytes currently buffered in the receive buffer (`byteRcvBuf`).
    pub recv_buffer_bytes: i32,
}

impl Stats {
    /// Project libsrt's raw performance monitor struct into a [`Stats`].
    pub(crate) fn from_raw(raw: &srt_io_sys::SRT_TRACEBSTATS) -> Self {
        Stats {
            rtt_ms: raw.msRTT,
            bandwidth_mbps: raw.mbpsBandwidth,
            send_rate_mbps: raw.mbpsSendRate,
            recv_rate_mbps: raw.mbpsRecvRate,
            packets_sent: raw.pktSentTotal,
            packets_received: raw.pktRecvTotal,
            packets_lost: i64::from(raw.pktRcvLossTotal),
            packets_retransmitted: i64::from(raw.pktRetransTotal),
            packets_sent_dropped: i64::from(raw.pktSndDropTotal),
            packets_recv_dropped: i64::from(raw.pktRcvDropTotal),
            bytes_sent: raw.byteSentTotal,
            bytes_received: raw.byteRecvTotal,
            bytes_retransmitted: raw.byteRetransTotal,
            send_buffer_packets: raw.pktSndBuf,
            send_buffer_bytes: raw.byteSndBuf,
            recv_buffer_packets: raw.pktRcvBuf,
            recv_buffer_bytes: raw.byteRcvBuf,
        }
    }
}
