use crate::traits::clock::Clock;
use crate::types::Bar;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RegimeState {
    Trending,
    Rotational,
    Volatile,
    #[default]
    Unknown,
}

pub trait RegimeDetector: Send {
    fn update(&mut self, bar: &Bar, clock: &dyn Clock) -> RegimeState;
    fn current(&self) -> RegimeState;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_unknown() {
        assert_eq!(RegimeState::default(), RegimeState::Unknown);
    }
}
