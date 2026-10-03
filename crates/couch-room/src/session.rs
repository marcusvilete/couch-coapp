//! Session and seat state, owned by the session's host.

use std::time::Instant;

use couch_proto::ids::{MemberId, Permissions, SeatEpoch, SeatId, SessionId};
use serde::{Deserialize, Serialize};

/// How many players can control the game at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionMode {
    /// One seat. The host starts with it and can pass it to any viewer.
    SinglePlayer,
}

impl SessionMode {
    pub fn seat_count(self) -> u8 {
        match self {
            Self::SinglePlayer => 1,
        }
    }
}

/// Why a session ended. Sent to every viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EndReason {
    HostStopped,
    GameExited,
    SourceLost,
    HostLeftRoom,
    HostShutdown,
    Error,
}

/// Who drives a seat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SeatHolder {
    /// The host's own devices drive the game; the virtual pad sits neutral.
    Host,
    Member(MemberId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeatStatus {
    Active,
    /// The holder's connection dropped. The seat returns to the host at `until`
    /// unless they come back first.
    Suspended {
        until: Instant,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seat {
    pub id: SeatId,
    pub holder: SeatHolder,
    pub epoch: SeatEpoch,
    pub permissions: Permissions,
    pub status: SeatStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: SessionId,
    pub host: MemberId,
    pub mode: SessionMode,
    pub seats: Vec<Seat>,
}

impl Session {
    /// A new session with every seat held by the host.
    pub fn new(id: SessionId, host: MemberId, mode: SessionMode) -> Self {
        let seats = (0..mode.seat_count())
            .map(|i| Seat {
                id: SeatId(i),
                holder: SeatHolder::Host,
                epoch: SeatEpoch(0),
                permissions: Permissions::GUEST_DEFAULT,
                status: SeatStatus::Active,
            })
            .collect();
        Self {
            id,
            host,
            mode,
            seats,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_player_starts_with_the_host_holding_one_seat() {
        let host = MemberId([1; 32]);
        let session = Session::new(SessionId(7), host, SessionMode::SinglePlayer);
        assert_eq!(session.seats.len(), 1);
        assert_eq!(session.seats[0].holder, SeatHolder::Host);
        assert_eq!(session.seats[0].status, SeatStatus::Active);
    }
}
