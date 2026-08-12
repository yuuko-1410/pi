use pi_coding_agent::modes::interactive::components::diff::{render_diff, RenderDiffOptions};

#[test]
fn probe() {
    let result = render_diff("-1 old line\n+1 new line\n", RenderDiffOptions::default());
    assert!(result.contains("old"));
    assert!(result.contains("new"));
    assert!(result.contains("line"));
}
