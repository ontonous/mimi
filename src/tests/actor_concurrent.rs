use super::*;

use crate::runtime::{
    actor_test_pause_after_pin, actor_test_pin_reached, mimi_actor_call, mimi_actor_drop,
    mimi_actor_id, mimi_actor_spawn, mimi_actor_spawn_detached, mimi_actor_system_kill,
};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};

// SAFETY: FFI 回调必须保持 extern "C" ABI；runtime 以函数指针调用本回调，
// 测试仅验证 actor 生命周期，不触碰共享状态，无内存安全风险。
unsafe extern "C" fn actor_dispatch(
    _method_id: i32,
    _fields: *mut c_void,
    _args: *const c_void,
    _args_size: i64,
    result: *mut c_void,
    result_size: *mut i64,
) {
    // SAFETY: mimi_actor_spawn 契约保证 result 指向 ≥8 字节可写缓冲（i64 载荷），
    // result_size 指向有效 i64；本回调仅写入两个标量，不越界。
    unsafe {
        *(result as *mut i64) = 42;
        *result_size = std::mem::size_of::<i64>() as i64;
    }
}

#[test]
fn actor_call_pins_lifetime_while_drop_detaches_handle() {
    actor_test_pause_after_pin(true);

    let fields = 0u8;
    let handle = unsafe {
        mimi_actor_spawn(
            &fields as *const u8 as *const c_void,
            1,
            Some(actor_dispatch),
        )
    };
    assert!(!handle.is_null());

    let call_handle = handle as usize;
    let call = std::thread::spawn(move || {
        let mut result = 0i64;
        let size = unsafe {
            mimi_actor_call(
                call_handle as *mut c_void,
                0,
                std::ptr::null(),
                0,
                &mut result as *mut i64 as *mut c_void,
            )
        };
        (size, result)
    });

    // This pause is after the registry lock yielded an Arc but before the first
    // actor-state access. The old live-check/raw-deref ordering freed here.
    while !actor_test_pin_reached() {
        std::thread::yield_now();
    }

    let drop_started = Arc::new(Barrier::new(2));
    let drop_finished = Arc::new(AtomicBool::new(false));
    let drop_handle = handle as usize;
    let drop_started_thread = Arc::clone(&drop_started);
    let drop_finished_thread = Arc::clone(&drop_finished);
    let dropper = std::thread::spawn(move || {
        drop_started_thread.wait();
        unsafe {
            mimi_actor_drop(drop_handle as *mut c_void);
        }
        drop_finished_thread.store(true, Ordering::Release);
    });
    drop_started.wait();

    unsafe {
        while mimi_actor_id(handle) != 0 {
            std::thread::yield_now();
        }
    }

    // Once detached, new calls must fail while the already-pinned call remains valid.
    let mut detached_result = 0i64;
    assert_eq!(
        unsafe {
            mimi_actor_call(
                handle,
                0,
                std::ptr::null(),
                0,
                &mut detached_result as *mut i64 as *mut c_void,
            )
        },
        0
    );
    // The handle is detached above, but the dropper is a separate thread.
    // Join it before observing the completion flag so this test proves the
    // drop completed instead of racing the scheduler.
    dropper.join().unwrap();
    assert!(drop_finished.load(Ordering::Acquire));
    actor_test_pause_after_pin(false);
    assert_eq!(call.join().unwrap(), (8, 42));
    assert!(drop_finished.load(Ordering::Acquire));
}

#[test]
fn actor_call_drop_l3_stress() {
    // SAFETY: 同 actor_dispatch——result/result_size 由 spawn 契约保证有效。
    unsafe extern "C" fn dispatch(
        _method_id: i32,
        _fields: *mut c_void,
        _args: *const c_void,
        _args_size: i64,
        result: *mut c_void,
        result_size: *mut i64,
    ) {
        // SAFETY: 同 actor_dispatch——result 为 ≥8 字节缓冲，result_size 有效。
        unsafe {
            *(result as *mut i64) = 7;
            *result_size = std::mem::size_of::<i64>() as i64;
        }
    }

    for _ in 0..256 {
        let fields = 0u8;
        let handle =
            unsafe { mimi_actor_spawn(&fields as *const u8 as *const c_void, 1, Some(dispatch)) };
        assert!(!handle.is_null());
        let barrier = Arc::new(Barrier::new(5));
        let mut callers = Vec::new();
        for _ in 0..4 {
            let call_handle = handle as usize;
            let barrier = Arc::clone(&barrier);
            callers.push(std::thread::spawn(move || {
                barrier.wait();
                let mut result = 0i64;
                let _ = unsafe {
                    mimi_actor_call(
                        call_handle as *mut c_void,
                        0,
                        std::ptr::null(),
                        0,
                        &mut result as *mut i64 as *mut c_void,
                    )
                };
            }));
        }
        barrier.wait();
        unsafe {
            mimi_actor_drop(handle);
        }
        for caller in callers {
            caller.join().unwrap();
        }
    }
}

// SAFETY: FFI 回调签名声明，runtime 仅以函数指针调用；测试 harness 专用，
// 回调内 spawn 的 child actor 生命周期由 spawn_detached/join 契约管理。
unsafe extern "C" fn lifecycle_dispatch(
    method_id: i32,
    _fields: *mut c_void,
    _args: *const c_void,
    _args_size: i64,
    result: *mut c_void,
    result_size: *mut i64,
) {
    // SAFETY: result 缓冲 ≥8 字节（i64 载荷契约）；child handle 装箱为 i64
    // 后由 runtime 持有，不在此处解引用，无悬垂风险。
    unsafe {
        let child_fields = 0u8;
        let child = if method_id == 1 {
            mimi_actor_spawn_detached(
                &child_fields as *const u8 as *const c_void,
                1,
                Some(actor_dispatch),
            )
        } else {
            mimi_actor_spawn(
                &child_fields as *const u8 as *const c_void,
                1,
                Some(actor_dispatch),
            )
        };
        *(result as *mut i64) = child as i64;
        *result_size = std::mem::size_of::<i64>() as i64;
    }
}

#[test]
fn actor_system_kill_cascades_but_preserves_detached_child() {
    let fields = 0u8;
    let parent = unsafe {
        mimi_actor_spawn(
            &fields as *const u8 as *const c_void,
            1,
            Some(lifecycle_dispatch),
        )
    };
    assert!(!parent.is_null());

    let spawn_child = |method_id| {
        let mut child = 0i64;
        assert_eq!(
            unsafe {
                mimi_actor_call(
                    parent,
                    method_id,
                    std::ptr::null(),
                    0,
                    &mut child as *mut i64 as *mut c_void,
                )
            },
            8
        );
        child as *mut c_void
    };
    let child = spawn_child(0);
    let detached = spawn_child(1);

    unsafe {
        mimi_actor_system_kill(parent);
    }

    let mut result = 0i64;
    unsafe {
        assert_eq!(mimi_actor_id(parent), 0);
        assert_eq!(mimi_actor_id(child), 0);
    }
    assert_eq!(
        unsafe {
            mimi_actor_call(
                child,
                0,
                std::ptr::null(),
                0,
                &mut result as *mut i64 as *mut c_void,
            )
        },
        0
    );
    unsafe {
        assert_ne!(mimi_actor_id(detached), 0);
    }
    assert_eq!(
        unsafe {
            mimi_actor_call(
                detached,
                0,
                std::ptr::null(),
                0,
                &mut result as *mut i64 as *mut c_void,
            )
        },
        8
    );
    assert_eq!(result, 42);
    unsafe {
        mimi_actor_drop(detached);
    }
}

#[test]
fn actor_sync_method_call() {
    let src = r#"
actor Counter {
    count: i32 = 0;

    func get() -> i32 {
        return self.count;
    }
}

func main() -> i32 {
    let c = Counter.spawn();
    c.get()
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn actor_await_method() {
    let src = r#"
actor Counter {
    count: i32 = 0;

    func increment() {
        self.count = self.count + 1;
    }

    func get() -> i32 {
        return self.count;
    }
}

func main() -> i32 {
    let c = Counter.spawn();
    c.increment();
    c.get()
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(1));
}

#[test]
fn actor_spawn_creates_handle() {
    let src = r#"
actor Worker {
    val: i32 = 42;

    func work() -> i32 {
        return self.val;
    }
}

func main() -> i32 {
    let w = Worker.spawn();
    w.work()
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn actor_multiple_methods() {
    let src = r#"
actor Calc {
    x: i32 = 3;
    y: i32 = 7;

    func add() -> i32 {
        return self.x + self.y;
    }

    func mul() -> i32 {
        return self.x * self.y;
    }
}

func main() -> i32 {
    let c = Calc.spawn();
    let a = c.add();
    let m = c.mul();
    a + m
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(31));
}

#[test]
fn actor_await_multiple_methods() {
    let src = r#"
actor Counter {
    count: i32 = 0;

    func increment() {
        self.count = self.count + 1;
    }

    func get() -> i32 {
        return self.count;
    }
}

func main() -> i32 {
    let c = Counter.spawn();
    c.increment();
    c.increment();
    c.get()
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(2));
}

#[test]
fn actor_state_persistence() {
    let src = r#"
actor State {
    counter: i32 = 100;

    func get() -> i32 {
        return self.counter;
    }
}

func main() -> i32 {
    let s = State.spawn();
    s.get()
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(100));
}

#[test]
fn actor_in_function() {
    let src = r#"
actor Box {
    val: i32 = 55;

    func unwrap() -> i32 {
        return self.val;
    }
}

func make_and_use() -> i32 {
    let b = Box.spawn();
    b.unwrap()
}

func main() -> i32 {
    make_and_use()
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(55));
}

#[test]
fn actor_spawn_method_return() {
    let src = r#"
actor Adder {
    base: i32 = 10;

    func add(x: i32) -> i32 {
        return self.base + x;
    }
}

func main() -> i32 {
    let a = Adder.spawn();
    a.add(5)
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(15));
}
