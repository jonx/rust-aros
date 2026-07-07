//! time for AROS, over posixc `clock_gettime`.
//!
//! `Instant` uses CLOCK_MONOTONIC (0), `SystemTime` uses CLOCK_REALTIME (2). AROS
//! `struct timespec` is `{ time_t tv_sec; long tv_nsec; }` and `time_t` is 32-bit,
//! so on LP64 it is `{ i32; i64 }` (4 bytes pad before tv_nsec). We carry the value
//! as a `Duration` and let `Duration` do the arithmetic, so this file is small and
//! the overflow/borrow logic is the library's, not ours.
use crate::time::Duration;
use crate::{fmt, io};

mod c {
    #[repr(C)]
    pub struct timespec {
        pub tv_sec: i32,
        pub tv_nsec: i64,
    }
    unsafe extern "C" {
        pub fn clock_gettime(clk: i32, tp: *mut timespec) -> i32;
    }
    pub const CLOCK_MONOTONIC: i32 = 0;
    pub const CLOCK_REALTIME: i32 = 2;
}

fn now(clk: i32) -> Duration {
    let mut ts = c::timespec { tv_sec: 0, tv_nsec: 0 };
    let r = unsafe { c::clock_gettime(clk, &mut ts) };
    if r != 0 || ts.tv_sec < 0 {
        return Duration::ZERO;
    }
    let nsec = (ts.tv_nsec as u64 % 1_000_000_000) as u32;
    Duration::new(ts.tv_sec as u64, nsec)
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Instant(Duration);

impl Instant {
    pub fn now() -> Instant {
        Instant(now(c::CLOCK_MONOTONIC))
    }
    pub fn checked_sub_instant(&self, other: &Instant) -> Option<Duration> {
        self.0.checked_sub(other.0)
    }
    pub fn checked_add_duration(&self, other: &Duration) -> Option<Instant> {
        Some(Instant(self.0.checked_add(*other)?))
    }
    pub fn checked_sub_duration(&self, other: &Duration) -> Option<Instant> {
        Some(Instant(self.0.checked_sub(*other)?))
    }
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SystemTime(Duration);

pub const UNIX_EPOCH: SystemTime = SystemTime(Duration::ZERO);

impl SystemTime {
    pub const MAX: SystemTime = SystemTime(Duration::MAX);
    pub const MIN: SystemTime = SystemTime(Duration::ZERO);

    #[allow(dead_code)]
    pub fn new(tv_sec: i64, tv_nsec: i64) -> Result<SystemTime, io::Error> {
        let nsec = (tv_nsec.rem_euclid(1_000_000_000)) as u32;
        Ok(SystemTime(Duration::new(tv_sec.max(0) as u64, nsec)))
    }
    pub fn now() -> SystemTime {
        SystemTime(now(c::CLOCK_REALTIME))
    }
    pub fn sub_time(&self, other: &SystemTime) -> Result<Duration, Duration> {
        if self.0 >= other.0 { Ok(self.0 - other.0) } else { Err(other.0 - self.0) }
    }
    pub fn checked_add_duration(&self, other: &Duration) -> Option<SystemTime> {
        Some(SystemTime(self.0.checked_add(*other)?))
    }
    pub fn checked_sub_duration(&self, other: &Duration) -> Option<SystemTime> {
        Some(SystemTime(self.0.checked_sub(*other)?))
    }

    /// Seconds + nanoseconds since the epoch, for the `fs` pal's `utimes` glue.
    #[allow(dead_code)]
    pub(crate) fn to_secs_nanos(&self) -> (i64, i64) {
        (self.0.as_secs() as i64, self.0.subsec_nanos() as i64)
    }
}

impl fmt::Debug for SystemTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SystemTime")
            .field("secs_since_epoch", &self.0.as_secs())
            .field("nanos", &self.0.subsec_nanos())
            .finish()
    }
}
