//! A-03 状态机全转移表单测（host-runtime.md §3.2 转移表；protocol.md §5.1 状态图）：
//! 每条合法转移与非法转移（返回 `None`）全覆盖。

use ab_host::{PluginProcessState, SmEvent, StateMachine};

/// 参考转移表（测试内固化，与协议状态图逐条对应）。
fn expected(state: PluginProcessState, ev: SmEvent) -> Option<PluginProcessState> {
    use PluginProcessState as S;
    use SmEvent as E;
    Some(match (state, ev) {
        (S::Discovered, E::SpawnRequested) => S::Spawning,
        (S::Discovered, E::ShutdownRequested) => S::Shutdown,

        (S::Spawning, E::SpawnFailed) => S::Crashed,
        (S::Spawning, E::Initialized) => S::Initializing,
        (S::Spawning, E::ShutdownRequested) => S::Draining,
        (S::Spawning, E::ExitConfirmed) => S::Crashed,

        (S::Initializing, E::Initialized) => S::Ready,
        (S::Initializing, E::ShutdownRequested) => S::Draining,
        (S::Initializing, E::ExitConfirmed) => S::Crashed,
        (S::Initializing, E::HeartbeatMissed) => S::Timeout,
        (S::Initializing, E::ProtocolFatalError) => S::Crashed,

        (S::Ready, E::LoadStarted) => S::Loading,
        (S::Ready, E::ParseStarted) => S::Parsing,
        (S::Ready, E::ShutdownRequested) => S::Draining,
        (S::Ready, E::ExitConfirmed) => S::Crashed,
        (S::Ready, E::ProtocolFatalError) => S::Crashed,

        (S::Loading, E::LoadFinished) => S::Ready,
        (S::Loading, E::ShutdownRequested) => S::Draining,
        (S::Loading, E::ExitConfirmed) => S::Crashed,
        (S::Loading, E::HeartbeatMissed) => S::Timeout,
        (S::Loading, E::ProtocolFatalError) => S::Crashed,

        (S::Parsing, E::ParseFinished) => S::Ready,
        (S::Parsing, E::ShutdownRequested) => S::Draining,
        (S::Parsing, E::ExitConfirmed) => S::Crashed,
        (S::Parsing, E::HeartbeatMissed) => S::Timeout,
        (S::Parsing, E::ProtocolFatalError) => S::Crashed,

        (S::Draining, E::ExitConfirmed) => S::Shutdown,
        (S::Draining, E::ProtocolFatalError) => S::Shutdown,

        // 吸收态与其余组合全为 ✗。
        _ => return None,
    })
}

/// 驱动状态机到目标状态（每次从 Discovered 走一条合法路径）。
fn drive_to(target: PluginProcessState) -> StateMachine {
    use PluginProcessState as S;
    let mut sm = StateMachine::new();
    let steps: &[SmEvent] = match target {
        S::Discovered => &[],
        S::Spawning => &[SmEvent::SpawnRequested],
        S::Initializing => &[SmEvent::SpawnRequested, SmEvent::Initialized],
        S::Ready => &[
            SmEvent::SpawnRequested,
            SmEvent::Initialized,
            SmEvent::Initialized,
        ],
        S::Loading => &[
            SmEvent::SpawnRequested,
            SmEvent::Initialized,
            SmEvent::Initialized,
            SmEvent::LoadStarted,
        ],
        S::Parsing => &[
            SmEvent::SpawnRequested,
            SmEvent::Initialized,
            SmEvent::Initialized,
            SmEvent::ParseStarted,
        ],
        S::Draining => &[
            SmEvent::SpawnRequested,
            SmEvent::Initialized,
            SmEvent::Initialized,
            SmEvent::ShutdownRequested,
        ],
        S::Shutdown => &[
            SmEvent::SpawnRequested,
            SmEvent::Initialized,
            SmEvent::Initialized,
            SmEvent::ShutdownRequested,
            SmEvent::ExitConfirmed,
        ],
        S::Crashed => &[SmEvent::SpawnRequested, SmEvent::SpawnFailed],
        S::Timeout => &[
            SmEvent::SpawnRequested,
            SmEvent::Initialized,
            SmEvent::HeartbeatMissed,
        ],
    };
    for step in steps {
        assert!(
            sm.apply(*step).is_some(),
            "drive_to({target:?}): step {step:?} must be legal"
        );
    }
    assert_eq!(sm.state(), target, "driven to target state");
    sm
}

#[test]
fn full_transition_table_covered() {
    let states = [
        PluginProcessState::Discovered,
        PluginProcessState::Spawning,
        PluginProcessState::Initializing,
        PluginProcessState::Ready,
        PluginProcessState::Loading,
        PluginProcessState::Parsing,
        PluginProcessState::Draining,
        PluginProcessState::Shutdown,
        PluginProcessState::Crashed,
        PluginProcessState::Timeout,
    ];
    let events = [
        SmEvent::SpawnRequested,
        SmEvent::SpawnFailed,
        SmEvent::Initialized,
        SmEvent::LoadStarted,
        SmEvent::LoadFinished,
        SmEvent::ParseStarted,
        SmEvent::ParseFinished,
        SmEvent::ShutdownRequested,
        SmEvent::ExitConfirmed,
        SmEvent::HeartbeatMissed,
        SmEvent::ProtocolFatalError,
    ];

    let mut legal = 0;
    let mut illegal = 0;
    for &state in &states {
        let mut sm = drive_to(state);
        for &ev in &events {
            let before = sm.state();
            let got = sm.apply(ev);
            match (got, expected(state, ev)) {
                (Some(to), Some(exp)) => {
                    assert_eq!(to, exp, "from {state:?} on {ev:?}");
                    assert_eq!(sm.state(), exp);
                    legal += 1;
                    // 转移成功后恢复到目标状态，继续测下一个事件。
                    sm = drive_to(state);
                }
                (None, None) => {
                    assert_eq!(sm.state(), before, "illegal transition must not mutate");
                    illegal += 1;
                }
                (Some(to), None) => {
                    panic!("impl allows illegal transition {state:?} + {ev:?} -> {to:?}")
                }
                (None, Some(exp)) => {
                    panic!("impl rejects legal transition {state:?} + {ev:?} -> {exp:?}")
                }
            }
        }
    }
    // 合法转移数（host-runtime.md §3.2 表）与非法组合数自检。
    assert_eq!(legal, 28, "all legal transitions covered");
    assert_eq!(illegal, 10 * 11 - 28, "all illegal combinations covered");
}
