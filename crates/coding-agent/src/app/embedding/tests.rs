use super::*;

#[cfg(test)]
mod deepseek_provider_tests {
    use super::*;

    #[test]
    fn deepseek_responses_exposes_effort_levels_and_off() {
        let model = ai::api::model::get_model("deepseek", "deepseek-v4-flash")
            .expect("DeepSeek V4 Flash is in the catalog");
        let capability = thinking_capability(&model);

        assert!(capability.supported);
        assert!(capability.can_disable);
        assert!(capability.supports(CodingAgentThinkingLevel::Off));
        assert!(capability.supports(CodingAgentThinkingLevel::Low));
        assert!(capability.supports(CodingAgentThinkingLevel::XHigh));
    }
}
