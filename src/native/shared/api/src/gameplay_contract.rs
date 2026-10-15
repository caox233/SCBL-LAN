//! Explicit compatibility identity for account UI versus a running game.
//! A mode declaration is not remote code attestation. Admission must additionally
//! bind it to the authenticated game run and immutable native session identity.

pub const ACCOUNT_PRODUCT: &str = "scbl-lan/1/official";
pub const GAME_INSTANCE_HEADER: &str = "x-scbl-game-instance";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GameplayMode { Official, Mod }

impl GameplayMode {
    pub const fn product(self) -> &'static str {
        match self {
            Self::Official => "scbl-lan/2/game/official",
            Self::Mod => "scbl-lan/2/game/mod",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientContract { Account, Game(GameplayMode) }

impl ClientContract {
    pub fn parse(product: &str) -> Option<Self> {
        match product {
            ACCOUNT_PRODUCT => Some(Self::Account),
            "scbl-lan/2/game/official" => Some(Self::Game(GameplayMode::Official)),
            "scbl-lan/2/game/mod" => Some(Self::Game(GameplayMode::Mod)),
            _ => None,
        }
    }
}

/// Opaque per-process nonce, not a secret or a user/account identifier.
/// Reject malformed/zero values rather than silently using a shared identity.
pub fn valid_game_instance(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && value.bytes().any(|b| b != b'0')
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn products_are_disjoint_and_versioned() {
        assert_eq!(ClientContract::parse(ACCOUNT_PRODUCT),Some(ClientContract::Account));
        for mode in [GameplayMode::Official,GameplayMode::Mod] {
            assert_eq!(ClientContract::parse(mode.product()),Some(ClientContract::Game(mode)));
        }
        assert_ne!(GameplayMode::Official.product(),GameplayMode::Mod.product());
    }
    #[test] fn unknown_or_ambiguous_products_fail_closed() {
        for v in ["", "scbl-lan/1/mod", "scbl-lan/2/game/MOD", " scbl-lan/2/game/mod", "scbl-lan/2/game/mod ", "scbl-lan/3/game/mod", "scbl-lan/2/game/mod/extra"] {
            assert_eq!(ClientContract::parse(v),None,"{v}");
        }
    }
    #[test] fn instance_is_bounded_canonical_and_nonzero() {
        assert!(valid_game_instance("0123456789abcdef0123456789abcdef"));
        for s in ["", "abc", "00000000000000000000000000000000", "0123456789ABCDEF0123456789ABCDEF", "0123456789abcdef0123456789abcdeg", "01234567-89ab-cdef-0123-456789abcdef"] {
            assert!(!valid_game_instance(s),"{s}");
        }
    }
}
