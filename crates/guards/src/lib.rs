//! Guards around the loop: the stop check (M3), the time budget and the loop
//! detector (M4).

mod loops;
mod stop;
mod time;

pub use loops::{LoopDetector, LOOP_MESSAGE};
pub use stop::{StopCheck, StopVerdict};
pub use time::{TimeBudget, TimeEvent};
