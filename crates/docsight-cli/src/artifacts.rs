use super::Command;
use docsight_core::{DocsightError, validate_artifact_paths, validate_new_artifact_directory};

pub(crate) fn validate_command_artifacts(command: &Command) -> Result<(), DocsightError> {
    match command {
        Command::Render {
            path, out, trace, ..
        } => {
            let mut outputs = vec![out.as_path()];
            if let Some(trace) = trace {
                outputs.push(trace.as_path());
            }
            validate_artifact_paths(&[path.as_path()], &outputs)
        }
        Command::Crop { path, out, .. } => {
            validate_artifact_paths(&[path.as_path()], &[out.as_path()])
        }
        Command::Bundle { path, out, .. } => {
            validate_artifact_paths(&[path.as_path()], &[out.as_path()])
        }
        Command::Diff { out_dir, .. } => {
            if let Some(out_dir) = out_dir {
                validate_new_artifact_directory(out_dir)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
