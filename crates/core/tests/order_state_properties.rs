use futures_bmad_core::OrderState;
use proptest::prelude::*;

fn arb_order_state() -> impl Strategy<Value = OrderState> {
    prop_oneof![
        Just(OrderState::Idle),
        Just(OrderState::Submitted),
        Just(OrderState::Confirmed),
        Just(OrderState::PartialFill),
        Just(OrderState::Filled),
        Just(OrderState::Rejected),
        Just(OrderState::Cancelled),
        Just(OrderState::PendingCancel),
        Just(OrderState::Uncertain),
        Just(OrderState::PendingRecon),
        Just(OrderState::Resolved),
    ]
}

proptest! {
    #[test]
    fn try_transition_matches_can_transition(
        from in arb_order_state(),
        to in arb_order_state(),
    ) {
        let can = from.can_transition_to(to);
        let result = from.try_transition(to);
        prop_assert_eq!(can, result.is_ok());
        if can {
            prop_assert_eq!(result.unwrap(), to);
        }
    }

    #[test]
    fn terminal_states_cannot_transition(
        to in arb_order_state(),
    ) {
        let terminals = [
            OrderState::Filled,
            OrderState::Rejected,
            OrderState::Cancelled,
            OrderState::Resolved,
        ];
        for terminal in &terminals {
            prop_assert!(!terminal.can_transition_to(to),
                "terminal state {:?} should not transition to {:?}", terminal, to);
        }
    }
}
