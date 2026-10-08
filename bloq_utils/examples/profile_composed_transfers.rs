//! Count work for a composed prefix transfer; no timing or compiler-speedup claim.
//! Usage: `profile_composed_transfers WIDTH`.

use bloq_utils::boolean::{
    BooleanDecisionDiagram, BooleanLimits, BooleanRow, DECISION_TRUE as ONE,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let width: usize = std::env::args()
        .nth(1)
        .expect("width")
        .parse()
        .expect("width");
    assert!(width > 0);
    let mut diagram = BooleanDecisionDiagram::with_limits(BooleanLimits::UNLIMITED);
    diagram.start_witness_tape([], width.checked_mul(2).expect("operation count"))?;
    let mut row = BooleanRow::default();
    let mut prefixes = Vec::new();
    for column in 0..width {
        let mut local = BooleanRow::from_terms(ONE, [(column, ONE)]);
        local.defer_coefficients(&mut diagram)?;
        row.xor_scaled(&local, ONE, &mut diagram)?;
        prefixes.push(row.clone());
    }
    let construction_steps = diagram.steps();
    let tape_collection_work = diagram.witness_collection_work();
    for (column, row) in prefixes.iter().enumerate() {
        assert_eq!(row.probe_coefficients(&[column], &mut diagram)?, [ONE]);
        assert!(row.terms().is_empty());
    }
    let local_query_steps = diagram.steps() - construction_steps;
    let start = diagram.steps();
    for row in &prefixes {
        assert_eq!(row.probe_coefficients(&[0], &mut diagram)?, [ONE]);
    }
    let oldest_query_steps = diagram.steps() - start;
    let mut batched_diagram = diagram.clone();
    let mut batched = prefixes.clone();
    let start = batched_diagram.steps();
    batched_diagram.extend_witness_front(&[0], &mut batched)?;
    let batched_oldest_steps = batched_diagram.steps() - start;
    assert!(
        batched
            .iter()
            .all(|row| row.get(0) == ONE && row.terms().len() == 1)
    );
    let start = diagram.steps();
    let mut expanded_terms = 0;
    for (column, mut row) in prefixes.into_iter().enumerate() {
        row.expand_coefficients(&mut diagram)?;
        assert_eq!(row.terms().len(), column + 1);
        assert!(row.terms().iter().all(|&(_, value)| value == ONE));
        expanded_terms += row.terms().len();
    }
    let expansion_steps = diagram.steps() - start;
    println!(
        "{{\"width\":{width},\"local_terms\":{width},\"expanded_terms\":{expanded_terms},\"construction_steps\":{construction_steps},\"tape_collection_work\":{tape_collection_work},\"local_query_steps\":{local_query_steps},\"oldest_query_steps\":{oldest_query_steps},\"batched_oldest_steps\":{batched_oldest_steps},\"expansion_steps\":{expansion_steps}}}"
    );
    Ok(())
}
