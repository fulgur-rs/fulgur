//! GVL (Giant VM Lock) 解放ヘルパ。
//!
//! magnus 0.7 は GVL 解放 API を持たないため、rb-sys の
//! `rb_thread_call_without_gvl` を直接呼ぶ。解放中のスレッドで
//! Ruby VM に触れると UB になるため、closure は Rust-only データで動く
//! ようにする (`&Engine`, `String` など)。
//!
//! # 設計メモ
//!
//! - `body` は `fn` pointer (キャプチャ無し)。キャプチャが必要なら
//!   `Data` に必要な値を詰めて渡す。現在の `render_html` 呼び出しでは
//!   `Args { engine: *const Engine, html: String }` 相当を渡す。
//! - ubf (unblock function) は `None`。Ruby プロセスが signal を受けても
//!   GVL 再取得まで保留される。v0.5.0 では interruptable rendering を
//!   実装しないので OK。
//! - `body` 内で panic が発生した場合、`extern "C"` 境界を unwind すると
//!   UB になる (rustc docs 参照)。shim 内で `catch_unwind` で捕捉し、
//!   GVL 再取得後に `resume_unwind` で伝播させる。

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

/// `body` を GVL 解放状態で実行する。
///
/// # Safety (呼び出し側契約)
///
/// `body` 内で Ruby VM API を呼んではならない。`Value`, `RString`, `Ruby::get()`
/// のような Ruby 依存の型・関数はすべて禁止。純粋な Rust データ (`String`,
/// `&Engine` など) のみ操作すること。
///
/// `body` 内で panic が発生した場合は `catch_unwind` で捕捉され、GVL 再取得後に
/// `resume_unwind` で呼び出し側スレッドに伝播する。unwind が `extern "C"` 境界を
/// 越えないため UB にはならない。
pub fn without_gvl<Data, Ret>(data: Data, body: fn(Data) -> Ret) -> Ret {
    struct Payload<D, R> {
        data: Option<D>,
        body: fn(D) -> R,
        result: Option<std::thread::Result<R>>,
    }

    unsafe extern "C" fn shim<D, R>(arg: *mut c_void) -> *mut c_void {
        // SAFETY: arg は `Payload<D, R>` への mutable pointer (caller が作る)。
        // GVL 解放中、他の Ruby スレッドはこの領域を触れない (payload は
        // caller のスタックにあり、caller は block する)。
        let p = unsafe { &mut *(arg as *mut Payload<D, R>) };
        let data = p.data.take().expect("payload data taken twice");
        let body = p.body;
        // panic を catch_unwind で捕捉し、extern "C" 境界を越える unwind (UB) を防ぐ。
        // AssertUnwindSafe は `body: fn(Data) -> Ret` が UnwindSafe でないケース
        // (e.g., `&Engine` を含む Data) でも、panic 後は payload を触らないため安全。
        p.result = Some(catch_unwind(AssertUnwindSafe(move || body(data))));
        std::ptr::null_mut()
    }

    let mut payload: Payload<Data, Ret> = Payload {
        data: Some(data),
        body,
        result: None,
    };
    // SAFETY: shim は payload を mutable 参照するが、rb_thread_call_without_gvl
    // は caller を block し、shim が終わるまで payload は生存する。ubf=None
    // なので shim は必ず完走し、payload.result が Some になる。
    unsafe {
        rb_sys::rb_thread_call_without_gvl(
            Some(shim::<Data, Ret>),
            &mut payload as *mut _ as *mut c_void,
            None,
            std::ptr::null_mut(),
        );
    }
    match payload
        .result
        .expect("rb_thread_call_without_gvl did not invoke shim")
    {
        Ok(v) => v,
        // GVL 再取得済みの状態で panic を呼び出し側に伝播する。
        Err(payload) => resume_unwind(payload),
    }
}
