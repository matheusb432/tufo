use embed_resource::CompilationResult;
use rspolib::AsBytes as _;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let catalog = rspolib::pofile("lang/pt_BR/LC_MESSAGES/tufo-gui.po")?;
    if !catalog.untranslated_entries().is_empty() || !catalog.fuzzy_entries().is_empty() {
        return Err("the pt-BR catalog contains untranslated or fuzzy entries".into());
    }
    let output = std::path::PathBuf::from(std::env::var("OUT_DIR")?);
    std::fs::write(output.join("pt_BR.mo"), catalog.as_bytes())?;
    // The Rust side shares the catalog and looks its messages up without a context.
    let config = slint_build::CompilerConfiguration::new()
        .with_bundled_translations("lang")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("ui/app.slint", config)?;
    println!("cargo::rerun-if-changed=lang");
    // Explorer, the Start menu, and the taskbar take the executable's icon from this resource.
    println!("cargo::rerun-if-changed=icons/tufo.ico");
    println!("cargo::rerun-if-changed=icons/tufo-tray.ico");
    match embed_resource::compile("tufo-gui.rc", embed_resource::NONE) {
        CompilationResult::NotAttempted(reason) => {
            println!("cargo::warning=tufo-gui.exe gets no icon: {reason}");
        }
        result => result.manifest_required()?,
    }
    Ok(())
}
