use std::{collections::BTreeSet, path::Path};

use anyhow::{Context as _, Result, bail};
use cargo_metadata::{DependencyKind, Metadata, MetadataCommand};

struct PackagePolicy {
    package: &'static str,
    dependencies: &'static [&'static str],
}

const PACKAGE_POLICIES: [PackagePolicy; 12] = [
    PackagePolicy {
        package: "field",
        dependencies: &[],
    },
    PackagePolicy {
        package: "tufo-local-transport",
        dependencies: &[],
    },
    PackagePolicy {
        package: "tufo-models",
        dependencies: &[],
    },
    PackagePolicy {
        package: "tufo-wire",
        dependencies: &["tufo-models"],
    },
    PackagePolicy {
        package: "tufo-application",
        dependencies: &["error-meta", "tufo-models", "tufo-wire"],
    },
    PackagePolicy {
        package: "tufo-infra",
        dependencies: &["tufo-application", "tufo-models", "tufo-wire"],
    },
    PackagePolicy {
        package: "tufo-migrator",
        dependencies: &["tufo-infra"],
    },
    PackagePolicy {
        package: "tufo-server",
        dependencies: &[
            "tufo-application",
            "tufo-infra",
            "tufo-local-transport",
            "tufo-models",
            "tufo-wire",
        ],
    },
    PackagePolicy {
        package: "tufo-client",
        dependencies: &["tufo-local-transport", "tufo-models", "tufo-wire"],
    },
    PackagePolicy {
        package: "tufo-cli",
        dependencies: &["tufo-client", "tufo-models"],
    },
    PackagePolicy {
        package: "tufo-gui",
        dependencies: &[
            "tufo-application",
            "tufo-client",
            "tufo-local-transport",
            "tufo-models",
        ],
    },
    PackagePolicy {
        package: "xtask",
        dependencies: &[],
    },
];

pub(crate) fn check(workspace_root: &Path) -> Result<()> {
    let manifest = workspace_root.join("Cargo.toml");
    let metadata = MetadataCommand::new()
        .manifest_path(&manifest)
        .no_deps()
        .other_options(vec!["--locked".to_owned()])
        .exec()
        .with_context(|| format!("read locked Cargo metadata from {}", manifest.display()))?;
    let violations = collect_violations(&metadata);

    if violations.is_empty() {
        return Ok(());
    }
    for violation in &violations {
        eprintln!("{violation}");
    }
    bail!("check-architecture found {} violation(s)", violations.len())
}

fn collect_violations(metadata: &Metadata) -> Vec<String> {
    let workspace_packages = metadata
        .workspace_packages()
        .into_iter()
        .map(|package| package.name.as_str())
        .collect::<BTreeSet<_>>();
    let mut violations = Vec::new();

    for package_name in &workspace_packages {
        if !PACKAGE_POLICIES
            .iter()
            .any(|policy| policy.package == *package_name)
        {
            violations.push(format!(
                "Cargo.toml: workspace package {package_name} has no architecture policy"
            ));
        }
    }

    for policy in &PACKAGE_POLICIES {
        let Some(package) = metadata
            .workspace_packages()
            .into_iter()
            .find(|package| package.name == policy.package)
        else {
            violations.push(format!(
                "Cargo.toml: architecture policy refers to missing workspace package {}",
                policy.package
            ));
            continue;
        };
        violations.extend(package_violations(
            metadata,
            &workspace_packages,
            package,
            policy,
        ));
    }

    violations.sort();
    violations.dedup();
    violations
}

fn package_violations(
    metadata: &Metadata,
    workspace_packages: &BTreeSet<&str>,
    package: &cargo_metadata::Package,
    policy: &PackagePolicy,
) -> Vec<String> {
    let manifest = package
        .manifest_path
        .strip_prefix(&metadata.workspace_root)
        .unwrap_or(&package.manifest_path);
    let actual = package
        .dependencies
        .iter()
        .filter(|dependency| {
            matches!(
                dependency.kind,
                DependencyKind::Normal | DependencyKind::Build
            ) && (workspace_packages.contains(dependency.name.as_str())
                || dependency.name == "error-meta")
        })
        .map(|dependency| dependency.name.as_str())
        .collect::<BTreeSet<_>>();
    let expected = policy.dependencies.iter().copied().collect::<BTreeSet<_>>();
    let mut violations = Vec::new();

    for dependency in expected.difference(&actual) {
        violations.push(format!(
            "{manifest}: {} must depend on package {dependency}",
            policy.package
        ));
    }
    for dependency in actual.difference(&expected) {
        violations.push(format!(
            "{manifest}: {} -> {dependency} is outside the allowed dependency direction",
            policy.package
        ));
    }
    violations
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, process::Command};

    use cargo_metadata::MetadataCommand;

    use super::collect_violations;

    #[test]
    fn accepts_the_requested_layer_graph() {
        let workspace = tempfile::tempdir().unwrap();
        write_workspace(
            workspace.path(),
            &[
                ("field", ""),
                ("tufo-models", ""),
                (
                    "tufo-wire",
                    "[dependencies]\ntufo-models = { path = \"../tufo-models\" }\n",
                ),
                (
                    "tufo-application",
                    "[dependencies]\nerror-meta = { path = \"../../external/error-meta\" }\ntufo-models = { path = \"../tufo-models\" }\ntufo-wire = { path = \"../tufo-wire\" }\n",
                ),
                ("tufo-local-transport", ""),
                (
                    "tufo-infra",
                    "[dependencies]\ntufo-application = { path = \"../tufo-application\" }\ntufo-models = { path = \"../tufo-models\" }\ntufo-wire = { path = \"../tufo-wire\" }\n",
                ),
                (
                    "tufo-server",
                    "[dependencies]\ntufo-application = { path = \"../tufo-application\" }\ntufo-infra = { path = \"../tufo-infra\" }\ntufo-local-transport = { path = \"../tufo-local-transport\" }\ntufo-models = { path = \"../tufo-models\" }\ntufo-wire = { path = \"../tufo-wire\" }\n",
                ),
                (
                    "tufo-migrator",
                    "[dependencies]\ntufo-infra = { path = \"../tufo-infra\" }\n",
                ),
                (
                    "tufo-client",
                    "[dependencies]\ntufo-local-transport = { path = \"../tufo-local-transport\" }\ntufo-models = { path = \"../tufo-models\" }\ntufo-wire = { path = \"../tufo-wire\" }\n",
                ),
                (
                    "tufo-gui",
                    "[dependencies]\ntufo-client = { path = \"../tufo-client\" }\ntufo-local-transport = { path = \"../tufo-local-transport\" }\ntufo-models = { path = \"../tufo-models\" }\ntufo-application = { path = \"../tufo-application\" }\n",
                ),
                (
                    "tufo-cli",
                    "[dependencies]\ntufo-client = { path = \"../tufo-client\" }\ntufo-models = { path = \"../tufo-models\" }\n",
                ),
                ("xtask", ""),
            ],
        );
        generate_lockfile(workspace.path());

        assert_eq!(read_violations(workspace.path()), Vec::<String>::new());
    }

    #[test]
    fn rejects_missing_and_outward_edges() {
        let workspace = tempfile::tempdir().unwrap();
        write_workspace(
            workspace.path(),
            &[
                ("field", ""),
                (
                    "tufo-models",
                    "[dependencies]\nfield = { path = \"../../shared/field\" }\nerror-meta = { path = \"../../external/error-meta\" }\n",
                ),
                ("tufo-wire", ""),
                (
                    "tufo-application",
                    "[dependencies]\nerror-meta = { path = \"../../external/error-meta\" }\ntufo-models = { path = \"../tufo-models\" }\ntufo-wire = { path = \"../tufo-wire\" }\n",
                ),
                ("tufo-local-transport", ""),
                (
                    "tufo-infra",
                    "[dependencies]\ntufo-application = { path = \"../tufo-application\" }\ntufo-models = { path = \"../tufo-models\" }\ntufo-wire = { path = \"../tufo-wire\" }\n",
                ),
                (
                    "tufo-server",
                    "[dependencies]\ntufo-application = { path = \"../tufo-application\" }\ntufo-infra = { path = \"../tufo-infra\" }\ntufo-local-transport = { path = \"../tufo-local-transport\" }\ntufo-models = { path = \"../tufo-models\" }\ntufo-wire = { path = \"../tufo-wire\" }\n",
                ),
                (
                    "tufo-migrator",
                    "[dependencies]\ntufo-infra = { path = \"../tufo-infra\" }\n",
                ),
                (
                    "tufo-client",
                    "[dependencies]\ntufo-local-transport = { path = \"../tufo-local-transport\" }\ntufo-models = { path = \"../tufo-models\" }\ntufo-wire = { path = \"../tufo-wire\" }\n",
                ),
                (
                    "tufo-gui",
                    "[dependencies]\ntufo-client = { path = \"../tufo-client\" }\ntufo-local-transport = { path = \"../tufo-local-transport\" }\ntufo-models = { path = \"../tufo-models\" }\ntufo-application = { path = \"../tufo-application\" }\n",
                ),
                (
                    "tufo-cli",
                    "[dependencies]\ntufo-client = { path = \"../tufo-client\" }\ntufo-models = { path = \"../tufo-models\" }\n",
                ),
                ("xtask", ""),
            ],
        );
        generate_lockfile(workspace.path());

        let violations = read_violations(workspace.path());

        assert!(
            violations
                .iter()
                .any(|violation| { violation.contains("tufo-models -> field is outside") })
        );
        assert!(violations.iter().any(|violation| {
            violation.contains("tufo-wire must depend on package tufo-models")
        }));
        assert!(
            violations
                .iter()
                .any(|violation| { violation.contains("tufo-models -> error-meta is outside") })
        );
    }

    #[test]
    fn checks_build_edges_and_ignores_development_edges() {
        let workspace = tempfile::tempdir().unwrap();
        write_workspace(
            workspace.path(),
            &[
                (
                    "tufo-models",
                    "[build-dependencies]\nfield = { path = \"../../shared/field\" }\n[dev-dependencies]\nerror-meta = { path = \"../../external/error-meta\" }\ntufo-gui = { path = \"../tufo-gui\" }\n",
                ),
                ("field", ""),
                (
                    "tufo-wire",
                    "[dependencies]\ntufo-models = { path = \"../tufo-models\" }\n",
                ),
                (
                    "tufo-application",
                    "[dependencies]\nerror-meta = { path = \"../../external/error-meta\" }\ntufo-models = { path = \"../tufo-models\" }\ntufo-wire = { path = \"../tufo-wire\" }\n",
                ),
                ("tufo-local-transport", ""),
                (
                    "tufo-infra",
                    "[dependencies]\ntufo-application = { path = \"../tufo-application\" }\ntufo-models = { path = \"../tufo-models\" }\ntufo-wire = { path = \"../tufo-wire\" }\n",
                ),
                (
                    "tufo-server",
                    "[dependencies]\ntufo-application = { path = \"../tufo-application\" }\ntufo-infra = { path = \"../tufo-infra\" }\ntufo-local-transport = { path = \"../tufo-local-transport\" }\ntufo-models = { path = \"../tufo-models\" }\ntufo-wire = { path = \"../tufo-wire\" }\n",
                ),
                (
                    "tufo-migrator",
                    "[dependencies]\ntufo-infra = { path = \"../tufo-infra\" }\n",
                ),
                (
                    "tufo-client",
                    "[dependencies]\ntufo-local-transport = { path = \"../tufo-local-transport\" }\ntufo-models = { path = \"../tufo-models\" }\ntufo-wire = { path = \"../tufo-wire\" }\n",
                ),
                (
                    "tufo-gui",
                    "[dependencies]\ntufo-client = { path = \"../tufo-client\" }\ntufo-local-transport = { path = \"../tufo-local-transport\" }\ntufo-models = { path = \"../tufo-models\" }\ntufo-application = { path = \"../tufo-application\" }\n",
                ),
                (
                    "tufo-cli",
                    "[dependencies]\ntufo-client = { path = \"../tufo-client\" }\ntufo-models = { path = \"../tufo-models\" }\n",
                ),
                ("xtask", ""),
            ],
        );
        generate_lockfile(workspace.path());

        let violations = read_violations(workspace.path());

        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("tufo-models -> field is outside"));
    }

    #[test]
    fn rejects_workspace_packages_without_a_policy() {
        let workspace = tempfile::tempdir().unwrap();
        write_workspace(
            workspace.path(),
            &[
                ("field", ""),
                ("tufo-models", ""),
                (
                    "tufo-wire",
                    "[dependencies]\ntufo-models = { path = \"../tufo-models\" }\n",
                ),
                (
                    "tufo-application",
                    "[dependencies]\nerror-meta = { path = \"../../external/error-meta\" }\ntufo-models = { path = \"../tufo-models\" }\ntufo-wire = { path = \"../tufo-wire\" }\n",
                ),
                ("tufo-local-transport", ""),
                (
                    "tufo-infra",
                    "[dependencies]\ntufo-application = { path = \"../tufo-application\" }\ntufo-models = { path = \"../tufo-models\" }\ntufo-wire = { path = \"../tufo-wire\" }\n",
                ),
                (
                    "tufo-server",
                    "[dependencies]\ntufo-application = { path = \"../tufo-application\" }\ntufo-infra = { path = \"../tufo-infra\" }\ntufo-local-transport = { path = \"../tufo-local-transport\" }\ntufo-models = { path = \"../tufo-models\" }\ntufo-wire = { path = \"../tufo-wire\" }\n",
                ),
                (
                    "tufo-migrator",
                    "[dependencies]\ntufo-infra = { path = \"../tufo-infra\" }\n",
                ),
                (
                    "tufo-client",
                    "[dependencies]\ntufo-local-transport = { path = \"../tufo-local-transport\" }\ntufo-models = { path = \"../tufo-models\" }\ntufo-wire = { path = \"../tufo-wire\" }\n",
                ),
                (
                    "tufo-gui",
                    "[dependencies]\ntufo-client = { path = \"../tufo-client\" }\ntufo-local-transport = { path = \"../tufo-local-transport\" }\ntufo-models = { path = \"../tufo-models\" }\ntufo-application = { path = \"../tufo-application\" }\n",
                ),
                (
                    "tufo-cli",
                    "[dependencies]\ntufo-client = { path = \"../tufo-client\" }\ntufo-models = { path = \"../tufo-models\" }\n",
                ),
                ("xtask", ""),
                ("other-support", ""),
            ],
        );
        generate_lockfile(workspace.path());

        let violations = read_violations(workspace.path());

        assert!(violations.iter().any(|violation| {
            violation.contains("workspace package other-support has no architecture policy")
        }));
    }

    fn read_violations(root: &Path) -> Vec<String> {
        let metadata = MetadataCommand::new()
            .manifest_path(root.join("Cargo.toml"))
            .no_deps()
            .exec()
            .unwrap();
        collect_violations(&metadata)
    }

    fn write_workspace(root: &Path, packages: &[(&str, &str)]) {
        let external = root.join("external/error-meta");
        fs::create_dir_all(external.join("src")).unwrap();
        fs::write(
            external.join("Cargo.toml"),
            "[package]\nname = \"error-meta\"\nversion = \"0.1.0\"\n[workspace]\n",
        )
        .unwrap();
        fs::write(external.join("src/lib.rs"), "").unwrap();
        let members = packages
            .iter()
            .map(|(name, _)| format!("\"{}\"", member_path(name)))
            .collect::<Vec<_>>()
            .join(", ");
        fs::write(
            root.join("Cargo.toml"),
            format!("[workspace]\nmembers = [{members}]\nexclude = [\"external/error-meta\"]\nresolver = \"3\"\n"),
        )
        .unwrap();
        for (name, body) in packages {
            let package = root.join(member_path(name));
            fs::create_dir_all(package.join("src")).unwrap();
            fs::write(
                package.join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n{body}"
                ),
            )
            .unwrap();
            fs::write(package.join("src/lib.rs"), "").unwrap();
        }
    }

    fn member_path(name: &str) -> String {
        match name {
            "field" => "shared/field".to_owned(),
            "xtask" => "xtask".to_owned(),
            _ => format!("crates/{name}"),
        }
    }

    fn generate_lockfile(root: &Path) {
        let status = Command::new("cargo")
            .args(["generate-lockfile", "--offline"])
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success(), "generate temporary workspace lockfile");
    }
}
