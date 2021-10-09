// SPDX-License-Identifier: GPL-2.0

//! Tasks (threads and processes).
//!
//! C header: [`include/linux/sched.h`](../../../../include/linux/sched.h).

use crate::bindings;
use crate::c_str;
use crate::c_types;
use crate::str::CStr;
use core::{iter::Iterator, marker::PhantomData, mem::ManuallyDrop, ops::Deref};

/// Wraps the kernel's `struct task_struct`.
///
/// # Invariants
///
/// The pointer `Task::ptr` is non-null and valid. Its reference count is also non-zero.
///
/// # Examples
///
/// The following is an example of getting the PID of the current thread with zero additional cost
/// when compared to the C version:
///
/// ```
/// # use kernel::prelude::*;
/// use kernel::task::Task;
///
/// # fn test() {
/// Task::current().pid();
/// # }
/// ```
///
/// Getting the PID of the current process, also zero additional cost:
///
/// ```
/// # use kernel::prelude::*;
/// use kernel::task::Task;
///
/// # fn test() {
/// Task::current().group_leader().pid();
/// # }
/// ```
///
/// Getting the current task and storing it in some struct. The reference count is automatically
/// incremented when creating `State` and decremented when it is dropped:
///
/// ```
/// # use kernel::prelude::*;
/// use kernel::task::Task;
///
/// struct State {
///     creator: Task,
///     index: u32,
/// }
///
/// impl State {
///     fn new() -> Self {
///         Self {
///             creator: Task::current().clone(),
///             index: 0,
///         }
///     }
/// }
/// ```
pub struct Task {
    pub(crate) ptr: *mut bindings::task_struct,
}

// SAFETY: Given that the task is referenced, it is OK to send it to another thread.
unsafe impl Send for Task {}

// SAFETY: It's OK to access `Task` through references from other threads because we're either
// accessing properties that don't change (e.g., `pid`, `group_leader`) or that are properly
// synchronised by C code (e.g., `signal_pending`).
unsafe impl Sync for Task {}

/// The type of process identifiers (PIDs).
type Pid = bindings::pid_t;

// TODO: should live here? maybe under a uidgid module?
type Uid = bindings::uid_t;

impl Task {
    /// Returns a task reference for the currently executing task/thread.
    pub fn current<'a>() -> TaskRef<'a> {
        // SAFETY: Just an FFI call.
        let ptr = unsafe { bindings::get_current() };

        // SAFETY: If the current thread is still running, the current task is valid. Given
        // that `TaskRef` is not `Send`, we know it cannot be transferred to another thread (where
        // it could potentially outlive the caller).
        unsafe { TaskRef::from_ptr(ptr) }
    }

    /// Returns the group leader of the given task.
    pub fn group_leader(&self) -> TaskRef<'_> {
        // SAFETY: By the type invariant, we know that `self.ptr` is non-null and valid.
        let ptr = unsafe { (*self.ptr).group_leader };

        // SAFETY: The lifetime of the returned task reference is tied to the lifetime of `self`,
        // and given that a task has a reference to its group leader, we know it must be valid for
        // the lifetime of the returned task reference.
        unsafe { TaskRef::from_ptr(ptr) }
    }

    /// Returns the PID of the given task.
    // TODO: Consider adding pid by namespace?
    pub fn pid(&self) -> Pid {
        // SAFETY: By the type invariant, we know that `self.ptr` is non-null and valid.
        unsafe { (*self.ptr).pid }
    }

    // TODO: add documentation
    pub fn tgid(&self) -> Pid {
        unsafe { bindings::task_tgid_nr(self.ptr) }
    }

    // TODO: Consider adding uid by namespace?
    pub fn uid(&self) -> Uid {
        // Global wrapper for these?
        // Should use current_user_ns()?
        let init_user_ns = unsafe { &mut bindings::init_user_ns as *mut bindings::user_namespace };
        unsafe { bindings::from_kuid(init_user_ns, bindings::task_uid(self.ptr)) }
    }

    // TODO: Consider adding uid by namespace?
    pub fn euid(&self) -> Uid {
        let init_user_ns = unsafe { &mut bindings::init_user_ns as *mut bindings::user_namespace };
        // Should use from_kuid_munged?
        unsafe { bindings::from_kuid(init_user_ns, bindings::task_euid(self.ptr)) }
    }

    // Adding the macro get_task_comm to bindings doesn't work?
    // Get: ld.lld: error: undefined symbol: __compiletime_assert_316
    // Need to use task_lock (<linux/sched/task.h:166>) here? Or should locking be elsewhere?
    // __get_task_comm uses a spin lock which is bad practice in sleeping contexts
    pub fn comm(&self) -> &CStr {
        // let mut buf = [0; bindings::TASK_COMM_LEN as usize];
        // unsafe {
        //     bindings::__get_task_comm(
        //         &mut buf as *mut c_types::c_char,
        //         bindings::TASK_COMM_LEN as usize,
        //         self.ptr,
        //     )
        // };
        return unsafe { CStr::from_char_ptr(&(*self.ptr).comm as *const c_types::c_char) };
    }

    // Add SAFETY docs
    // What should happen here if it's a process?
    pub fn threads<'a>(&self) -> ThreadIterator<'a> {
        ThreadIterator {
            proc_task_ptr: self.ptr,
            thread_task_ptr: core::ptr::null_mut(),
            _not_send: PhantomData,
        }
    }

    /// Determines whether the given task has pending signals.
    pub fn signal_pending(&self) -> bool {
        // SAFETY: By the type invariant, we know that `self.ptr` is non-null and valid.
        unsafe { bindings::signal_pending(self.ptr) != 0 }
    }
}

impl PartialEq for Task {
    fn eq(&self, other: &Self) -> bool {
        self.ptr == other.ptr
    }
}

impl Eq for Task {}

impl Clone for Task {
    fn clone(&self) -> Self {
        // SAFETY: The type invariants guarantee that `self.ptr` has a non-zero reference count.
        unsafe { bindings::get_task_struct(self.ptr) };

        // INVARIANT: We incremented the reference count to account for the new `Task` being
        // created.
        Self { ptr: self.ptr }
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        // INVARIANT: We may decrement the refcount to zero, but the `Task` is being dropped, so
        // this is not observable.
        // SAFETY: The type invariants guarantee that `Task::ptr` has a non-zero reference count.
        unsafe { bindings::put_task_struct(self.ptr) };
    }
}

/// A wrapper for [`Task`] that doesn't automatically decrement the refcount when dropped.
///
/// We need the wrapper because [`ManuallyDrop`] alone would allow callers to call
/// [`ManuallyDrop::into_inner`]. This would allow an unsafe sequence to be triggered without
/// `unsafe` blocks because it would trigger an unbalanced call to `put_task_struct`.
///
/// We make this explicitly not [`Send`] so that we can use it to represent the current thread
/// without having to increment/decrement its reference count.
///
/// # Invariants
///
/// The wrapped [`Task`] remains valid for the lifetime of the object.
pub struct TaskRef<'a> {
    task: ManuallyDrop<Task>,
    _not_send: PhantomData<(&'a (), *mut ())>,
}

impl TaskRef<'_> {
    /// Constructs a new `struct task_struct` wrapper that doesn't change its reference count.
    ///
    /// # Safety
    ///
    /// The pointer `ptr` must be non-null and valid for the lifetime of the object.
    pub(crate) unsafe fn from_ptr(ptr: *mut bindings::task_struct) -> Self {
        Self {
            task: ManuallyDrop::new(Task { ptr }),
            _not_send: PhantomData,
        }
    }
}

// SAFETY: It is OK to share a reference to the current thread with another thread because we know
// the owner cannot go away while the shared reference exists (and `Task` itself is `Sync`).
unsafe impl Sync for TaskRef<'_> {}

impl Deref for TaskRef<'_> {
    type Target = Task;

    fn deref(&self) -> &Self::Target {
        self.task.deref()
    }
}

// Single iterator for all tasks? (processes & threads)
// Is PhantomData working here?
pub struct ProcessIterator<'a> {
    task_ptr: *mut bindings::task_struct,
    _not_send: PhantomData<(&'a (), *mut ())>,
}

impl ProcessIterator<'_> {
    pub fn new() -> Self {
        let init_task_ptr = unsafe { &mut bindings::init_task as *mut bindings::task_struct };
        unsafe { Self::from_ptr(init_task_ptr) }
    }

    // TODO: should this be pub? To allow the initial task to start the traversal from
    pub unsafe fn from_ptr(ptr: *mut bindings::task_struct) -> Self {
        ProcessIterator {
            task_ptr: ptr,
            _not_send: PhantomData,
        }
    }

    // TODO: consider from_task() equivalent? to allow traversal from a held task.
    // I guess we don't know if a task is always a process so we should check that
}

impl<'a> Iterator for ProcessIterator<'a> {
    type Item = TaskRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let next_task_ptr = unsafe { bindings::next_task(self.task_ptr) };
        // TODO: can this be a global? is there an abstraction we can use around mutable statics?
        let init_task_ptr = unsafe { &mut bindings::init_task as *mut bindings::task_struct };
        if next_task_ptr.is_null() || next_task_ptr == init_task_ptr {
            None
        } else {
            self.task_ptr = next_task_ptr;
            let task_ref = unsafe { TaskRef::from_ptr(next_task_ptr) };
            Some(task_ref)
        }
    }
}
pub struct ThreadIterator<'a> {
    proc_task_ptr: *mut bindings::task_struct,
    thread_task_ptr: *mut bindings::task_struct,
    _not_send: PhantomData<(&'a (), *mut ())>,
}

impl<'a> Iterator for ThreadIterator<'a> {
    type Item = TaskRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        // Should we include the process task within the thread tasks?
        if self.thread_task_ptr.is_null() {
            self.thread_task_ptr = self.proc_task_ptr;
            let task_ref = unsafe { TaskRef::from_ptr(self.thread_task_ptr) };
            return Some(task_ref);
        }
        let next_thread_ptr = unsafe { bindings::next_thread(self.thread_task_ptr) };
        if next_thread_ptr.is_null() || next_thread_ptr == self.proc_task_ptr {
            None
        } else {
            self.thread_task_ptr = next_thread_ptr;
            Some(unsafe { TaskRef::from_ptr(self.thread_task_ptr) })
        }
    }
}
