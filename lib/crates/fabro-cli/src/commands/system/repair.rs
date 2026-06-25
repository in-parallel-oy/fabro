use anyhow::{Result, bail};
use fabro_api::types;
use fabro_util::printer::Printer;
use serde::Serialize;

use crate::args::{SystemRepairArgs, SystemRepairCommand, SystemRepairRunsArgs};
use crate::command_context::CommandContext;
use crate::server_client::Client;
use crate::shared::print_json_pretty;

pub(super) async fn repair_command(
    args: &SystemRepairArgs,
    base_ctx: &CommandContext,
) -> Result<()> {
    match &args.command {
        SystemRepairCommand::Runs(args) => repair_runs_command(args, base_ctx).await,
    }
}

async fn repair_runs_command(args: &SystemRepairRunsArgs, base_ctx: &CommandContext) -> Result<()> {
    let ctx = base_ctx.with_connection(&args.connection)?;
    let server = ctx.server().await?;
    let response = server.get_system_repair_runs().await?;
    let result = if args.delete {
        let summary = delete_repair_runs(&response, server.as_ref(), args.yes).await;
        repair_runs_delete_from(&response, &summary, ctx.json_output(), ctx.printer())
    } else {
        repair_runs_from(&response, ctx.json_output(), ctx.printer())
    };
    result?;
    Ok(())
}

fn repair_runs_from(
    response: &types::SystemRepairRunsResponse,
    json_output: bool,
    printer: Printer,
) -> Result<()> {
    if json_output {
        print_json_pretty(response)?;
        return Ok(());
    }

    if response.runs.is_empty() {
        fabro_util::printout!(printer, "No run repair issues found.");
        return Ok(());
    }

    fabro_util::printout!(printer, "Unreadable runs:");
    for run in &response.runs {
        fabro_util::printout!(
            printer,
            "  {}  {}  {}",
            run.run_id,
            run.created_at.to_rfc3339(),
            run.error,
        );
    }

    fabro_util::printout!(printer, "");
    fabro_util::printout!(printer, "Delete with:");
    for run in &response.runs {
        fabro_util::printout!(printer, "  fabro rm --force {}", run.run_id);
    }
    Ok(())
}

async fn delete_repair_runs<'a>(
    response: &'a types::SystemRepairRunsResponse,
    server: &Client,
    yes: bool,
) -> RepairRunsDeleteSummary<'a> {
    let mut summary = RepairRunsDeleteSummary {
        dry_run: !yes,
        total_count: response.total_count,
        runs: &response.runs,
        deleted: Vec::new(),
        errors: Vec::new(),
    };

    if !yes {
        return summary;
    }

    for run in &response.runs {
        let run_id = match run.run_id.parse::<fabro_types::RunId>() {
            Ok(run_id) => run_id,
            Err(err) => {
                summary.errors.push(RepairRunsDeleteError {
                    run_id: run.run_id.clone(),
                    error: err.to_string(),
                });
                continue;
            }
        };

        match server.delete_store_run(&run_id, true).await {
            Ok(()) => summary.deleted.push(run.run_id.clone()),
            Err(err) => summary.errors.push(RepairRunsDeleteError {
                run_id: run.run_id.clone(),
                error: err.to_string(),
            }),
        }
    }

    summary
}

fn repair_runs_delete_from(
    response: &types::SystemRepairRunsResponse,
    summary: &RepairRunsDeleteSummary<'_>,
    json_output: bool,
    printer: Printer,
) -> Result<()> {
    if json_output {
        print_json_pretty(summary)?;
    } else if response.runs.is_empty() {
        fabro_util::printout!(printer, "No run repair issues found.");
    } else if summary.dry_run {
        print_unreadable_runs(response, printer);
        fabro_util::printout!(printer, "");
        fabro_util::printout!(
            printer,
            "{} unreadable run(s) would be deleted. Pass --delete --yes to confirm.",
            response.total_count
        );
    } else {
        for run_id in &summary.deleted {
            fabro_util::printout!(printer, "deleted: {run_id}");
        }
        for error in &summary.errors {
            fabro_util::printerr!(printer, "error: {}: {}", error.run_id, error.error);
        }
        fabro_util::printerr!(
            printer,
            "{} unreadable run(s) deleted.",
            summary.deleted.len()
        );
    }

    if !summary.errors.is_empty() {
        bail!("some unreadable runs could not be deleted");
    }
    Ok(())
}

fn print_unreadable_runs(response: &types::SystemRepairRunsResponse, printer: Printer) {
    fabro_util::printout!(printer, "Unreadable runs:");
    for run in &response.runs {
        fabro_util::printout!(
            printer,
            "  {}  {}  {}",
            run.run_id,
            run.created_at.to_rfc3339(),
            run.error,
        );
    }
}

#[derive(Serialize)]
struct RepairRunsDeleteSummary<'a> {
    dry_run: bool,
    total_count: i64,
    runs: &'a [types::SystemRepairRunIssue],
    deleted: Vec<String>,
    errors: Vec<RepairRunsDeleteError>,
}

#[derive(Serialize)]
struct RepairRunsDeleteError {
    run_id: String,
    error: String,
}
