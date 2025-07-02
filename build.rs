use vergen::BuildBuilder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let build = BuildBuilder::default().build_timestamp(true).build()?;

    vergen::Emitter::default()
        .add_instructions(&build)?
        .emit()?;

    Ok(())
}
