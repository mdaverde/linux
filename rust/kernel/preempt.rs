// SPDX-License-Identifier: GPL-2.0

//! Kernel preemption

use crate::bindings;

// Read's into current()'s preempt_count. Should preempt_count be
// a struct on Rust's side? Or something other than tagged pointers
// Should this be derived from a Task::task?
pub fn in_task() -> bool {
    unsafe { bindings::in_task() }
}
