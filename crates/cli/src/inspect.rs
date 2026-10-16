//! Questions asked of a component without running it: `inspect`, `diff`, and
//! `wit semver-check`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Subcommand};
use watoots::Host;
use watoots::imports::ImportDecision;

use crate::{Failure, Outcome, read, read_manifest, verdict};

#[derive(Args)]
pub(crate) struct InspectArgs {
    /// The component to inspect.
    component: PathBuf,
    /// Manifest to check against. Without one, everything is denied, which is
    /// the most useful default here: it shows the full bill.
    #[arg(short, long)]
    manifest: Option<PathBuf>,
    /// An interface the application serves, so it is not reported as a denial.
    /// Repeat for each one.
    #[arg(long = "provide", value_name = "INTERFACE")]
    provide: Vec<String>,
    /// List every import and its decision instead of summarising capabilities.
    #[arg(long)]
    imports: bool,
    /// Also check the component implements this world. A WIT file, a directory
    /// containing one, or a wasm-encoded WIT package.
    #[arg(long, value_name = "WIT")]
    targets: Option<PathBuf>,
    /// Which world in `--targets` to check against. Optional when the package
    /// declares exactly one.
    #[arg(long, value_name = "NAME", requires = "targets")]
    world: Option<String>,
}

#[derive(Args)]
pub(crate) struct DiffArgs {
    /// The build already deployed.
    previous: PathBuf,
    /// The build proposed to replace it.
    current: PathBuf,
    /// Manifest the plugin runs under. With one, this answers the question that
    /// actually matters: would the update still load, or would reload refuse
    /// it? Without one, capability changes are reported but not judged.
    #[arg(short, long)]
    manifest: Option<PathBuf>,
    /// An interface the application serves, so it is not reported as a denial.
    /// Repeat for each one.
    #[arg(long = "provide", value_name = "INTERFACE")]
    provide: Vec<String>,
}

#[derive(Subcommand)]
pub(crate) enum WitCommand {
    /// Check that one WIT package is a compatible successor to another.
    ///
    /// The predicate is wasm-tools': `--current` may have MORE imports and
    /// FEWER exports than `--previous`, and every type it keeps must be
    /// structurally identical. For a plugin world that is the useful
    /// direction -- a host may offer plugins more and demand less of them
    /// without breaking the ones already built.
    ///
    /// Takes two packages rather than a component, which is why it is not a
    /// flag on `inspect`. See ADR-0007.
    SemverCheck(SemverCheckArgs),
}

#[derive(Args)]
pub(crate) struct SemverCheckArgs {
    /// The released package.
    #[arg(long, value_name = "WIT")]
    previous: PathBuf,
    /// The package proposed to replace it.
    #[arg(long, value_name = "WIT")]
    current: PathBuf,
    /// Which world to compare. Optional when each package declares exactly one.
    #[arg(long, value_name = "NAME")]
    world: Option<String>,
}

/// A host for asking questions: the manifest, plus the interfaces the
/// application says it serves, and nothing that runs.
fn inspection_host(manifest: Option<&Path>, provide: &[String]) -> Result<Host, Failure> {
    let (manifest, _) = read_manifest(manifest)?;
    let mut builder = Host::builder().manifest(manifest);
    for interface in provide {
        builder = builder.provide_interface(interface.clone());
    }
    Ok(builder.build()?)
}

pub(crate) fn inspect(args: &InspectArgs) -> Outcome {
    let wasm = read(&args.component)?;
    let host = inspection_host(args.manifest.as_deref(), &args.provide)?;
    let report = host.inspect(&wasm)?;

    if args.imports {
        print!("{}", report.describe());
    } else {
        print!("{}", report.summarize(host.manifest()));
    }

    // Imports and exports are separate questions, so they get separate lines
    // and a failure in either fails the command.
    let mut ok = report.is_satisfied();
    if ok {
        println!("\nevery import is granted");
    } else {
        println!(
            "\n{} import(s) are not granted; `--imports` lists them individually",
            report.denied().count()
        );
    }

    if let Some(wit) = &args.targets {
        match host.check_targets(&wasm, wit, args.world.as_deref()) {
            Ok(()) => println!("exports match the world in {}", wit.display()),
            Err(err) => {
                println!("{}", err.message());
                ok = false;
            }
        }
    }

    verdict(ok)
}

/// What an update does with one import it did not have before, classified
/// once so the report and the verdict cannot disagree.
///
/// Three outcomes, not two, and they are fixed in different places: an
/// application interface is not a permission problem and must not be filed as
/// one -- the same rule `inspect` follows.
enum Gained<'a> {
    /// The manifest already covers it.
    AlreadyGranted,
    /// The application has to `provide_interface` it.
    AppServes,
    /// A line someone has to add to the manifest.
    NotGranted { key: &'a str },
}

impl<'a> Gained<'a> {
    fn of(decision: &'a ImportDecision) -> Self {
        if decision.granted {
            Self::AlreadyGranted
        } else if decision.served_by_application() {
            Self::AppServes
        } else {
            Self::NotGranted {
                key: decision.requirement.grant_key(),
            }
        }
    }
}

/// Compare two builds of the same plugin.
///
/// Deliberately not `wit semver-check`, which compares two WIT *packages* for
/// structural compatibility and is wrapped rather than reimplemented (ADR-0007).
/// This compares two compiled *components* and answers the question watoots is
/// uniquely placed to answer: does the new build ask for capabilities the old
/// one did not, and would the manifest it runs under still let it load?
///
/// That is the reload refusal, previewed. `Plugin::reload` re-runs the grant
/// check and refuses a replacement that wants more (ADR-0010), which is correct
/// and also the worst possible moment to find out.
pub(crate) fn diff(args: &DiffArgs) -> Outcome {
    let previous = read(&args.previous)?;
    let current = read(&args.current)?;
    let host = inspection_host(args.manifest.as_deref(), &args.provide)?;

    let describe = |wasm: &[u8]| -> Result<(Vec<ImportDecision>, Vec<String>), Failure> {
        Ok((host.inspect(wasm)?.decisions, host.export_functions(wasm)?))
    };
    let (previous_imports, previous_exports) = describe(&previous)?;
    let (current_imports, current_exports) = describe(&current)?;

    let names = |decisions: &[ImportDecision]| -> BTreeSet<String> {
        decisions.iter().map(|d| d.import.clone()).collect()
    };
    let (before, after) = (names(&previous_imports), names(&current_imports));

    // Requirement and grant come from the *current* build's report, because the
    // question is what the update needs, not what the old one was allowed.
    let gained: Vec<(&ImportDecision, Gained<'_>)> = current_imports
        .iter()
        .filter(|decision| !before.contains(&decision.import))
        .map(|decision| (decision, Gained::of(decision)))
        .collect();
    let lost: Vec<&String> = before.difference(&after).collect();

    println!("imports");
    if gained.is_empty() && lost.is_empty() {
        println!("  (unchanged)");
    }
    for (decision, change) in &gained {
        let name = &decision.import;
        match change {
            Gained::AlreadyGranted => println!("  + {name:<48} already granted"),
            Gained::AppServes => println!("  + {name:<48} your application must serve this"),
            // The grant key, not just the requirement: someone reading this is
            // about to edit a manifest and wants the line to write.
            Gained::NotGranted { key } => println!("  + {name:<48} NOT GRANTED -> {key}"),
        }
    }
    for name in &lost {
        println!("  - {name:<48} no longer needed");
    }

    let before_exports: BTreeSet<&String> = previous_exports.iter().collect();
    let after_exports: BTreeSet<&String> = current_exports.iter().collect();
    let added: Vec<&&String> = after_exports.difference(&before_exports).collect();
    let removed: Vec<&&String> = before_exports.difference(&after_exports).collect();

    println!("\nexports");
    if added.is_empty() && removed.is_empty() {
        println!("  (unchanged)");
    }
    for name in &added {
        println!("  + {name}");
    }
    for name in &removed {
        println!("  - {name:<48} callers of this break");
    }

    // Two independent ways an update can be a problem, reported separately
    // because they are fixed in different places: a new capability is a
    // manifest edit, a removed export is a change to the plugin or its callers.
    let count = |wanted: fn(&Gained<'_>) -> bool| gained.iter().filter(|(_, g)| wanted(g)).count();
    let to_serve = count(|g| matches!(g, Gained::AppServes));
    let ungranted = count(|g| matches!(g, Gained::NotGranted { .. }));
    println!();
    let mut ok = true;

    if to_serve > 0 {
        // Counted separately because the fix is a `provide_interface` call
        // in the application, not a line in the manifest.
        println!("{to_serve} new interface(s) your application must serve");
        ok = false;
    }

    if args.manifest.is_some() {
        if ungranted == 0 {
            println!("this update needs no new grants");
        } else {
            println!(
                "{ungranted} new import(s) the manifest does not grant; reload would refuse this build"
            );
            ok = false;
        }
    } else if gained.is_empty() {
        println!("no new imports; pass -m to check the update against a manifest");
    } else {
        println!(
            "{} new import(s); pass -m to find out whether a manifest grants them",
            gained.len()
        );
    }

    if !removed.is_empty() {
        println!(
            "{} export(s) removed; a host calling them breaks",
            removed.len()
        );
        ok = false;
    }

    verdict(ok)
}

/// `wasm-tools component semver-check`, wrapped rather than reimplemented.
pub(crate) fn semver_check(args: &SemverCheckArgs) -> Outcome {
    let mut resolve = wit_parser::Resolve::default();
    let mut load = |path: &Path| -> Result<_, Failure> {
        resolve
            .push_path(path)
            .map(|(package, _)| package)
            .map_err(|err| format!("{}: {err:#}", path.display()).into())
    };
    let previous = load(&args.previous)?;
    let current = load(&args.current)?;

    let select = |package, label: &str| -> Result<_, Failure> {
        resolve
            .select_world(&[package], args.world.as_deref())
            .map_err(|err| format!("{label}: {err:#}").into())
    };
    let previous_world = select(previous, "--previous")?;
    let current_world = select(current, "--current")?;

    match wit_component::semver_check(resolve, previous_world, current_world) {
        Ok(()) => {
            println!(
                "{} is a compatible successor to {}",
                args.current.display(),
                args.previous.display()
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => {
            println!("incompatible: {err:#}");
            Ok(ExitCode::FAILURE)
        }
    }
}
