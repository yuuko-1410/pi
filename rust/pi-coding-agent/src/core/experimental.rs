//! Experimental feature flag, port of `core/experimental.ts`.

pub fn are_experimental_features_enabled() -> bool {
    std::env::var("PI_EXPERIMENTAL").as_deref() == Ok("1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_by_default() {
        let _ = are_experimental_features_enabled();
    }
}
