# The syscall seam-trait pattern

*M1.4 / finding B-003 — the mock-injection pattern shared by
`framesage-sys::SysApi` and `framesage-etw::EtwSysCalls`, documented so
future adapters (Group B PresentMon subprocess driver, Group C session
recorder) use the same shape instead of inventing a third one.*

## The problem it solves

Almost everything interesting this project does is a Win32 syscall:
`SetProcessAffinityMask`, `StartTraceW`, `ControlTraceW`,
`OpenProcess`, SCM calls. Code that calls them directly is untestable
off a real, elevated Windows host — and untestable *deterministically*
even on one (you can't make `StartTraceW` return `ERROR_ACCESS_DENIED`
on demand to exercise the EDR-block path).

The seam trait moves every syscall behind one narrow trait object so
the decision logic above it runs against scripted fakes on any host,
while production wires in a zero-cost real implementation.

## The shape

Both existing seams follow the same five rules:

1. **One trait, whole surface.** The trait covers *every* syscall the
   subsystem makes — `SysApi` for the engine (~25 methods),
   `EtwSysCalls` for the ETW session (6 methods). No half-seams: if one
   call bypasses the trait, the paths that depend on it are untestable
   and the mock lies about coverage.
2. **Production impl is a ZST wrapper.** `RealSysApi` /
   `RealEtwSysCalls` are unit structs whose methods forward directly to
   the Win32 bindings. No state, no overhead; `Default + Clone` so
   generic call sites can construct them.
3. **Mock impl with scripted returns.** `MockSysApi` (in
   `crates/engine/src/lib.rs` tests) scripts per-method return values
   and records calls (e.g. `affinity_writes`); `MockEtwSysCalls`
   (`crates/etw/src/session.rs`) uses per-method FIFO queues
   (`expect_start_trace(ERROR_ACCESS_DENIED)`) plus armable panics for
   unwind-path tests. Pick queues when call *order* matters, plain
   scripted state when it doesn't.
4. **Injection at construction, generics or dyn as fits.** The engine
   holds `Arc<dyn SysApi>` (many call sites, object-safe surface); the
   ETW session is generic `EtwSession<S: EtwSysCalls>` with
   `RealEtwSysCalls` as the default type parameter (the consumer thread
   moves `S` by value, so no `Sync` bound is forced on mocks — see the
   `ConsumerState` docstring in `session.rs`).
5. **The trait is the *only* test seam for syscalls.** Auxiliary seams
   (e.g. the `build_gate` thread-local override behind the
   `test-override` feature) exist only where a `OnceLock` cache makes
   trait injection impossible; default to the trait.

## Unwind-safety caveat

If the subsystem crosses a `catch_unwind` boundary (the ETW consumer
thread does), the *production* impl must be `RefUnwindSafe` — pinned by
`assert_impl_all!` in `supervisor.rs`. `RefCell`-based mocks are exempt
only while panic injection fires before any `borrow_mut`; a mock that
panics mid-borrow must use a `Mutex` instead.

## Checklist for a new adapter (PresentMon, recorder, …)

- [ ] Define `trait XxxCalls` next to the subsystem, covering its whole
      external-process/syscall surface.
- [ ] `RealXxxCalls`: ZST forwarding impl, `Default + Clone`.
- [ ] `MockXxxCalls`: scripted returns (+ call recording for
      write-path assertions), `#[cfg(test)]` or a `test-…` feature if
      dependent crates' tests need it.
- [ ] Inject at construction; never call the real API directly from
      logic code.
- [ ] Decision-tree tests (happy path + every degradation mode) run on
      every host via the mock; only the thin `RealXxxCalls` layer needs
      the Windows runtime batch.
