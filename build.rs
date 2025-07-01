fn main() -> Result<(), Box<dyn std::error::Error>> {
    let build = vergen::BuildBuilder::all_build()?;

    vergen::Emitter::default()
        .add_instructions(&build)?
        .emit()?;

    Ok(())
}
