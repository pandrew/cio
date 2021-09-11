use std::{collections::HashMap, env, fs::File, sync::Arc};

use anyhow::Result;
use cio_api::{analytics::NewPageView, db::Database, functions::Function, swag_store::Order};
use docusign::DocuSign;
use dropshot::{
    endpoint, ApiDescription, ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpError, HttpResponseAccepted,
    HttpResponseOk, HttpServerStarter, Path, Query, RequestContext, TypedBody, UntypedBody,
};
use google_drive::Client as GoogleDrive;
use gusto_api::Client as Gusto;
use log::{info, warn};
use mailchimp_api::MailChimp;
use quickbooks::QuickBooks;
use ramp_api::Client as Ramp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use slack_chat_api::Slack;
use zoom_api::Client as Zoom;

use crate::github_types::GitHubWebhook;

pub async fn server(s: &crate::Server, logger: slog::Logger) -> Result<()> {
    /*
     * We must specify a configuration with a bind address.  We'll use 127.0.0.1
     * since it's available and won't expose this server outside the host.  We
     * request port 8080.
     */
    let config_dropshot = ConfigDropshot {
        bind_address: s.address.parse()?,
        request_body_max_bytes: 10737418240, // 10 Gigiabytes.
    };

    /*
     * For simplicity, we'll configure an "info"-level logger that writes to
     * stderr assuming that it's a terminal.
     */
    let config_logging = ConfigLogging::StderrTerminal {
        level: ConfigLoggingLevel::Info,
    };
    let log = config_logging.to_logger("webhooky-server")?;

    // Describe the API.
    let mut api = ApiDescription::new();
    /*
     * Register our endpoint and its handler function.  The "endpoint" macro
     * specifies the HTTP method and URI path that identify the endpoint,
     * allowing this metadata to live right alongside the handler function.
     */
    api.register(ping).unwrap();
    api.register(github_rate_limit).unwrap();
    api.register(listen_airtable_applicants_request_background_check_webhooks)
        .unwrap();
    api.register(listen_airtable_applicants_update_webhooks).unwrap();
    api.register(listen_airtable_assets_items_print_barcode_label_webhooks)
        .unwrap();
    api.register(listen_airtable_employees_print_home_address_label_webhooks)
        .unwrap();
    api.register(listen_airtable_shipments_inbound_create_webhooks).unwrap();
    api.register(listen_airtable_shipments_outbound_create_webhooks)
        .unwrap();
    api.register(listen_airtable_shipments_outbound_reprint_label_webhooks)
        .unwrap();
    api.register(listen_airtable_shipments_outbound_reprint_receipt_webhooks)
        .unwrap();
    api.register(listen_airtable_shipments_outbound_resend_shipment_status_email_to_recipient_webhooks)
        .unwrap();
    api.register(listen_airtable_shipments_outbound_schedule_pickup_webhooks)
        .unwrap();
    api.register(listen_airtable_swag_inventory_items_print_barcode_labels_webhooks)
        .unwrap();
    api.register(listen_analytics_page_view_webhooks).unwrap();
    api.register(listen_application_submit_requests).unwrap();
    api.register(listen_applicant_review_requests).unwrap();
    api.register(listen_application_files_upload_requests).unwrap();
    api.register(listen_auth_docusign_callback).unwrap();
    api.register(listen_auth_docusign_consent).unwrap();
    api.register(listen_auth_github_callback).unwrap();
    api.register(listen_auth_github_consent).unwrap();
    api.register(listen_auth_google_callback).unwrap();
    api.register(listen_auth_google_consent).unwrap();
    api.register(listen_auth_gusto_callback).unwrap();
    api.register(listen_auth_gusto_consent).unwrap();
    api.register(listen_auth_mailchimp_callback).unwrap();
    api.register(listen_auth_mailchimp_consent).unwrap();
    api.register(listen_auth_plaid_callback).unwrap();
    api.register(listen_auth_ramp_callback).unwrap();
    api.register(listen_auth_ramp_consent).unwrap();
    api.register(listen_auth_zoom_callback).unwrap();
    api.register(listen_auth_zoom_consent).unwrap();
    api.register(listen_auth_zoom_deauthorization).unwrap();
    api.register(listen_auth_slack_callback).unwrap();
    api.register(listen_auth_slack_consent).unwrap();
    api.register(listen_auth_quickbooks_callback).unwrap();
    api.register(listen_auth_quickbooks_consent).unwrap();
    api.register(listen_checkr_background_update_webhooks).unwrap();
    api.register(listen_docusign_envelope_update_webhooks).unwrap();
    api.register(listen_emails_incoming_sendgrid_parse_webhooks).unwrap();
    api.register(listen_google_sheets_edit_webhooks).unwrap();
    api.register(listen_google_sheets_row_create_webhooks).unwrap();
    api.register(listen_github_webhooks).unwrap();
    api.register(listen_mailchimp_mailing_list_webhooks).unwrap();
    api.register(listen_mailchimp_rack_line_webhooks).unwrap();
    api.register(listen_products_sold_count_requests).unwrap();
    api.register(listen_shippo_tracking_update_webhooks).unwrap();
    api.register(listen_slack_commands_webhooks).unwrap();
    api.register(listen_store_order_create).unwrap();
    api.register(ping_mailchimp_mailing_list_webhooks).unwrap();
    api.register(ping_mailchimp_rack_line_webhooks).unwrap();
    api.register(trigger_rfd_update_by_number).unwrap();
    api.register(trigger_sync_repos_create).unwrap();
    api.register(trigger_sync_rfds_create).unwrap();
    api.register(trigger_sync_travel_create).unwrap();
    api.register(trigger_sync_finance_create).unwrap();
    api.register(trigger_sync_shipments_create).unwrap();
    api.register(listen_get_function_by_uuid).unwrap();
    api.register(listen_get_function_logs_by_uuid).unwrap();
    api.register(api_get_schema).unwrap();

    // Create the API schema.
    let mut api_definition = &mut api.openapi(&"Webhooks API", &clap::crate_version!());
    api_definition = api_definition
        .description("Internal webhooks server for listening to several third party webhooks")
        .contact_url("https://oxide.computer")
        .contact_email("webhooks@oxide.computer");
    let schema = api_definition.json()?.to_string();

    if let Some(spec_file) = &s.spec_file {
        info!("writing OpenAPI spec to {}...", spec_file.to_str().unwrap());
        let mut buffer = File::create(spec_file)?;
        api_definition.write(&mut buffer)?;
    }

    /*
     * The functions that implement our API endpoints will share this context.
     */
    let api_context = Context::new(schema, logger).await;

    /*
     * TODO: Setup our cron jobs to run every few hours.
     */
    /*let mut interval = tokio::time::interval(std::time::Duration::from_secs(6 * 60 * 60));
    tokio::spawn(async move {
        // Make an infinite loop.
        loop {
            // Wait for our interval.
            interval.tick().await;

            // TODO: Stagger the starts.
            if let Err(e) = crate::handlers_cron::handle_reexec_cmd(&api_context, "sync-finance").await {
                sentry_anyhow::capture_anyhow(&e);
            }
            if let Err(e) = crate::handlers_cron::handle_reexec_cmd(&api_context, "sync-repos").await {
                sentry_anyhow::capture_anyhow(&e);
            }
            if let Err(e) = crate::handlers_cron::handle_reexec_cmd(&api_context, "sync-rfds").await {
                sentry_anyhow::capture_anyhow(&e);
            }
            if let Err(e) = crate::handlers_cron::handle_reexec_cmd(&api_context, "sync-shipments").await {
                sentry_anyhow::capture_anyhow(&e);
            }
            if let Err(e) = crate::handlers_cron::handle_reexec_cmd(&api_context, "sync-travel").await {
                sentry_anyhow::capture_anyhow(&e);
            }
        }
    });*/

    /*
     * Set up the server.
     */
    let server = HttpServerStarter::new(&config_dropshot, api, api_context, &log)?.start();

    // TODO: Listen for ^C and cleanup gracefully.
    server.await.unwrap();

    Ok(())
}

/**
 * Application-specific context (state shared by handler functions)
 */
pub struct Context {
    pub db: Database,

    pub sec: steno::SecClient,

    pub schema: String,
}

impl Context {
    /**
     * Return a new Context.
     */
    pub async fn new(schema: String, logger: slog::Logger) -> Context {
        let db = Database::new();

        let sec = steno::sec(logger, Arc::new(db.clone()));

        // Create the context.
        Context { db, sec, schema }
    }
}

/*
 * HTTP API interface
 */

/**
 * Return the OpenAPI schema in JSON format.
 */
#[endpoint {
    method = GET,
    path = "/",
}]
async fn api_get_schema(rqctx: Arc<RequestContext<Context>>) -> Result<HttpResponseOk<String>, HttpError> {
    let api_context = rqctx.context();

    Ok(HttpResponseOk(api_context.schema.to_string()))
}

/** Return pong. */
#[endpoint {
    method = GET,
    path = "/ping",
}]
async fn ping(_rqctx: Arc<RequestContext<Context>>) -> Result<HttpResponseOk<String>, HttpError> {
    Ok(HttpResponseOk("pong".to_string()))
}

#[derive(Deserialize, Serialize, Default, Clone, Debug, JsonSchema)]
pub struct CounterResponse {
    #[serde(default)]
    pub count: i32,
}

/** Return the count of products sold. */
#[endpoint {
    method = GET,
    path = "/products/sold/count",
}]
async fn listen_products_sold_count_requests(
    rqctx: Arc<RequestContext<Context>>,
) -> Result<HttpResponseOk<CounterResponse>, HttpError> {
    sentry::start_session();

    match crate::handlers::handle_products_sold_count(rqctx).await {
        Ok(r) => {
            sentry::end_session();
            Ok(HttpResponseOk(r))
        }
        // Send the error to sentry.
        Err(e) => {
            sentry::end_session();
            Err(handle_anyhow_err_as_http_err(e))
        }
    }
}

/** Listen for GitHub webhooks. */
#[endpoint {
    method = POST,
    path = "/github",
}]
async fn listen_github_webhooks(
    rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<GitHubWebhook>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers_github::handle_github(rqctx, body_param).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

#[derive(Deserialize, Debug, JsonSchema)]
pub struct RFDPathParams {
    pub num: i32,
}

/** Trigger an update for an RFD. */
#[endpoint {
    method = POST,
    path = "/rfd/{num}",
}]
async fn trigger_rfd_update_by_number(
    rqctx: Arc<RequestContext<Context>>,
    path_params: Path<RFDPathParams>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_rfd_update_by_number(rqctx, path_params).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/** Get our current GitHub rate limit. */
#[endpoint {
    method = GET,
    path = "/github/ratelimit",
}]
async fn github_rate_limit(rqctx: Arc<RequestContext<Context>>) -> Result<HttpResponseOk<GitHubRateLimit>, HttpError> {
    sentry::start_session();

    match crate::handlers::handle_github_rate_limit(rqctx).await {
        Ok(r) => {
            sentry::end_session();
            Ok(HttpResponseOk(r))
        }
        // Send the error to sentry.
        Err(e) => {
            sentry::end_session();
            Err(handle_anyhow_err_as_http_err(e))
        }
    }
}

/// A GitHub RateLimit
#[derive(Debug, Clone, Default, JsonSchema, Deserialize, Serialize)]
pub struct GitHubRateLimit {
    #[serde(default)]
    pub limit: u32,
    #[serde(default)]
    pub remaining: u32,
    #[serde(default)]
    pub reset: String,
}

/**
 * Listen for edits to our Google Sheets.
 * These are set up with a Google Apps script on the sheets themselves.
 */
#[endpoint {
    method = POST,
    path = "/google/sheets/edit",
}]
async fn listen_google_sheets_edit_webhooks(
    rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<GoogleSpreadsheetEditEvent>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_google_sheets_edit(rqctx, body_param).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/// A Google Sheet edit event.
#[derive(Debug, Clone, Default, JsonSchema, Deserialize, Serialize)]
pub struct GoogleSpreadsheetEditEvent {
    #[serde(default)]
    pub event: GoogleSpreadsheetEvent,
    #[serde(default)]
    pub spreadsheet: GoogleSpreadsheet,
}

#[derive(Debug, Clone, Default, JsonSchema, Deserialize, Serialize)]
pub struct GoogleSpreadsheetEvent {
    #[serde(default, skip_serializing_if = "String::is_empty", rename = "authMode")]
    pub auth_mode: String,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        rename = "oldValue",
        deserialize_with = "octorust::utils::deserialize_null_string::deserialize"
    )]
    pub old_value: String,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "octorust::utils::deserialize_null_string::deserialize"
    )]
    pub value: String,
    #[serde(default)]
    pub range: GoogleSpreadsheetRange,
    #[serde(default)]
    pub source: GoogleSpreadsheetSource,
    #[serde(default, skip_serializing_if = "String::is_empty", rename = "triggerUid")]
    pub trigger_uid: String,
    #[serde(default)]
    pub user: GoogleSpreadsheetUser,
    #[serde(default, skip_serializing_if = "HashMap::is_empty", rename = "namedValues")]
    pub named_values: HashMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
}

#[derive(Debug, Clone, Default, JsonSchema, Deserialize, Serialize)]
pub struct GoogleSpreadsheetRange {
    #[serde(default, rename = "columnEnd")]
    pub column_end: i64,
    #[serde(default, rename = "columnStart")]
    pub column_start: i64,
    #[serde(default, rename = "rowEnd")]
    pub row_end: i64,
    #[serde(default, rename = "rowStart")]
    pub row_start: i64,
}

#[derive(Debug, Clone, Default, JsonSchema, Deserialize, Serialize)]
pub struct GoogleSpreadsheetSource {}

#[derive(Debug, Clone, Default, JsonSchema, Deserialize, Serialize)]
pub struct GoogleSpreadsheetUser {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub email: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
}

#[derive(Debug, Clone, Default, JsonSchema, Deserialize, Serialize)]
pub struct GoogleSpreadsheet {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
}

/**
 * Listen for rows created in our Google Sheets.
 * These are set up with a Google Apps script on the sheets themselves.
 */
#[endpoint {
    method = POST,
    path = "/google/sheets/row/create",
}]
async fn listen_google_sheets_row_create_webhooks(
    rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<GoogleSpreadsheetRowCreateEvent>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_google_sheets_row_create(rqctx, body_param).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/// A Google Sheet row create event.
#[derive(Debug, Clone, Default, JsonSchema, Deserialize, Serialize)]
pub struct GoogleSpreadsheetRowCreateEvent {
    #[serde(default)]
    pub event: GoogleSpreadsheetEvent,
    #[serde(default)]
    pub spreadsheet: GoogleSpreadsheet,
}

/**
 * Listen for a button pressed to print a home address label for employees.
 */
#[endpoint {
    method = POST,
    path = "/airtable/employees/print_home_address_label",
}]
async fn listen_airtable_employees_print_home_address_label_webhooks(
    rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<AirtableRowEvent>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_airtable_employees_print_home_address_label(rqctx, body_param).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/**
 * Listen for a button pressed to print a barcode label for an asset item.
 */
#[endpoint {
    method = POST,
    path = "/airtable/assets/items/print_barcode_label",
}]
async fn listen_airtable_assets_items_print_barcode_label_webhooks(
    rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<AirtableRowEvent>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_airtable_assets_items_print_barcode_label(rqctx, body_param).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/**
 * Listen for a button pressed to print barcode labels for a swag inventory item.
 */
#[endpoint {
    method = POST,
    path = "/airtable/swag/inventory/items/print_barcode_labels",
}]
async fn listen_airtable_swag_inventory_items_print_barcode_labels_webhooks(
    rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<AirtableRowEvent>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_airtable_swag_inventory_items_print_barcode_labels(rqctx, body_param).await
    {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/**
 * Listen for a button pressed to request a background check for an applicant.
 */
#[endpoint {
    method = POST,
    path = "/airtable/applicants/request_background_check",
}]
async fn listen_airtable_applicants_request_background_check_webhooks(
    rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<AirtableRowEvent>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_airtable_applicants_request_background_check(rqctx, body_param).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/**
 * Listen for rows updated in our Airtable workspace.
 * These are set up with an Airtable script on the workspaces themselves.
 */
#[endpoint {
    method = POST,
    path = "/airtable/applicants/update",
}]
async fn listen_airtable_applicants_update_webhooks(
    rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<AirtableRowEvent>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_airtable_applicants_update(rqctx, body_param).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/**
 * Listen for rows created in our Airtable workspace.
 * These are set up with an Airtable script on the workspaces themselves.
 */
#[endpoint {
    method = POST,
    path = "/airtable/shipments/outbound/create",
}]
async fn listen_airtable_shipments_outbound_create_webhooks(
    rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<AirtableRowEvent>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_airtable_shipments_outbound_create(rqctx, body_param).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/// An Airtable row event.
#[derive(Debug, Clone, Default, JsonSchema, Deserialize, Serialize)]
pub struct AirtableRowEvent {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub record_id: String,
    #[serde(default)]
    pub cio_company_id: i32,
}

/**
 * Listen for a button pressed to reprint a label for an outbound shipment.
 */
#[endpoint {
    method = POST,
    path = "/airtable/shipments/outbound/reprint_label",
}]
async fn listen_airtable_shipments_outbound_reprint_label_webhooks(
    rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<AirtableRowEvent>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_airtable_shipments_outbound_reprint_label(rqctx, body_param).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/**
 * Listen for a button pressed to reprint a receipt for an outbound shipment.
 */
#[endpoint {
    method = POST,
    path = "/airtable/shipments/outbound/reprint_receipt",
}]
async fn listen_airtable_shipments_outbound_reprint_receipt_webhooks(
    rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<AirtableRowEvent>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_airtable_shipments_outbound_reprint_receipt(rqctx, body_param).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/**
 * Listen for a button pressed to resend a shipment status email to the recipient for an outbound shipment.
 */
#[endpoint {
    method = POST,
    path = "/airtable/shipments/outbound/resend_shipment_status_email_to_recipient",
}]
async fn listen_airtable_shipments_outbound_resend_shipment_status_email_to_recipient_webhooks(
    rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<AirtableRowEvent>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) =
        crate::handlers::handle_airtable_shipments_outbound_resend_shipment_status_email_to_recipient(rqctx, body_param)
            .await
    {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/**
 * Listen for a button pressed to schedule a pickup for an outbound shipment.
 */
#[endpoint {
    method = POST,
    path = "/airtable/shipments/outbound/schedule_pickup",
}]
async fn listen_airtable_shipments_outbound_schedule_pickup_webhooks(
    rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<AirtableRowEvent>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_airtable_shipments_outbound_schedule_pickup(rqctx, body_param).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/// A SendGrid incoming email event.
#[derive(Debug, Clone, Default, JsonSchema, Deserialize, Serialize)]
pub struct IncomingEmail {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub headers: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dkim: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub email: String,
    #[serde(default, skip_serializing_if = "String::is_empty", alias = "content-ids")]
    pub content_ids: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub to: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cc: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub html: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sender_ip: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub spam_report: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub envelope: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub attachments: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subject: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub spam_score: String,
    #[serde(default, skip_serializing_if = "String::is_empty", alias = "attachment-info")]
    pub attachment_info: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub charsets: String,
    #[serde(default, skip_serializing_if = "String::is_empty", alias = "SPF")]
    pub spf: String,
}

/**
 * Listen for emails coming inbound from SendGrid's parse API.
 * We use this for scanning for packages in emails.
 */
#[endpoint {
    method = POST,
    path = "/emails/incoming/sendgrid/parse",
}]
async fn listen_emails_incoming_sendgrid_parse_webhooks(
    rqctx: Arc<RequestContext<Context>>,
    body_param: UntypedBody,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_emails_incoming_sendgrid_parse(rqctx, body_param).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/**
 * Listen for applicant reviews being submitted for job applicants */
#[endpoint {
    method = POST,
    path = "/applicant/review/submit",
}]
async fn listen_applicant_review_requests(
    rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<cio_api::applicant_reviews::NewApplicantReview>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_applicant_review(rqctx, body_param).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/**
 * Listen for applications being submitted for incoming job applications */
#[endpoint {
    method = POST,
    path = "/application/submit",
}]
async fn listen_application_submit_requests(
    rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<cio_api::application_form::ApplicationForm>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_application_submit(rqctx, body_param).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/// Application file upload data.
#[derive(Debug, Clone, Default, JsonSchema, Deserialize, Serialize)]
pub struct ApplicationFileUploadData {
    #[serde(default)]
    pub cio_company_id: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub resume: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub materials: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub email: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub resume_contents: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub materials_contents: String,
}

/**
 * Listen for files being uploaded for incoming job applications */
#[endpoint {
    method = POST,
    path = "/application/files/upload",
}]
async fn listen_application_files_upload_requests(
    rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<ApplicationFileUploadData>,
) -> Result<HttpResponseOk<HashMap<String, String>>, HttpError> {
    sentry::start_session();

    match crate::handlers::handle_application_files_upload(rqctx, body_param).await {
        Ok(r) => {
            sentry::end_session();
            Ok(HttpResponseOk(r))
        }
        // Send the error to sentry.
        Err(e) => {
            sentry::end_session();
            Err(handle_anyhow_err_as_http_err(e))
        }
    }
}

/**
 * Listen for rows created in our Airtable workspace.
 * These are set up with an Airtable script on the workspaces themselves.
 */
#[endpoint {
    method = POST,
    path = "/airtable/shipments/inbound/create",
}]
async fn listen_airtable_shipments_inbound_create_webhooks(
    rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<AirtableRowEvent>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_airtable_shipments_inbound_create(rqctx, body_param).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/**
 * Listen for orders being created by the Oxide store.
 */
#[endpoint {
    method = POST,
    path = "/store/order",
}]
async fn listen_store_order_create(
    rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<Order>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_store_order_create(rqctx, body_param).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/**
 * Listen for shipment tracking updated from Shippo.
 */
#[endpoint {
    method = POST,
    path = "/shippo/tracking/update",
}]
async fn listen_shippo_tracking_update_webhooks(
    rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<serde_json::Value>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_shippo_tracking_update(rqctx, body_param).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/// A Shippo tracking update event.
#[derive(Debug, Clone, Default, JsonSchema, Deserialize, Serialize)]
pub struct ShippoTrackingUpdateEvent {
    #[serde(default)]
    pub data: shippo::TrackingStatus,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub event: String,
    #[serde(default)]
    pub test: bool,
}

/** Listen for updates to our checkr background checks. */
#[endpoint {
    method = POST,
    path = "/checkr/background/update",
}]
async fn listen_checkr_background_update_webhooks(
    rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<checkr::WebhookEvent>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_checkr_background_update(rqctx, body_param).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

#[derive(Debug, Clone, Default, JsonSchema, Deserialize, Serialize)]
pub struct UserConsentURL {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
}

#[derive(Debug, Clone, Default, JsonSchema, Deserialize, Serialize)]
pub struct AuthCallback {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
    /// The state that we had passed in through the user consent URL.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub state: String,
    #[serde(default, skip_serializing_if = "String::is_empty", rename = "realmId")]
    pub realm_id: String,
}

/** Get the consent URL for Google auth. */
#[endpoint {
    method = GET,
    path = "/auth/google/consent",
}]
async fn listen_auth_google_consent(
    _rqctx: Arc<RequestContext<Context>>,
) -> Result<HttpResponseOk<UserConsentURL>, HttpError> {
    sentry::start_session();

    // Initialize the Google client.
    // You can use any of the libs here, they all use the same endpoint
    // for tokens and we will send all the scopes.
    let g = GoogleDrive::new_from_env("", "").await;

    sentry::end_session();
    Ok(HttpResponseOk(UserConsentURL {
        url: g.user_consent_url(&cio_api::companies::get_google_scopes()),
    }))
}

/** Listen for callbacks to Google auth. */
#[endpoint {
    method = GET,
    path = "/auth/google/callback",
}]
async fn listen_auth_google_callback(
    rqctx: Arc<RequestContext<Context>>,
    query_args: Query<AuthCallback>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers_auth::handle_auth_google_callback(rqctx, query_args).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/** Get the consent URL for GitHub auth. */
#[endpoint {
    method = GET,
    path = "/auth/github/consent",
}]
async fn listen_auth_github_consent(
    _rqctx: Arc<RequestContext<Context>>,
) -> Result<HttpResponseOk<UserConsentURL>, HttpError> {
    sentry::start_session();

    sentry::end_session();
    Ok(HttpResponseOk(UserConsentURL {
        url: "https://github.com/apps/oxidecomputerbot/installations/new".to_string(),
    }))
}

/** Listen for callbacks to GitHub auth. */
#[endpoint {
    method = GET,
    path = "/auth/github/callback",
}]
async fn listen_auth_github_callback(
    _rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<serde_json::Value>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();
    let event = body_param.into_inner();

    warn!("github callback: {:?}", event);

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/** Get the consent URL for MailChimp auth. */
#[endpoint {
    method = GET,
    path = "/auth/mailchimp/consent",
}]
async fn listen_auth_mailchimp_consent(
    _rqctx: Arc<RequestContext<Context>>,
) -> Result<HttpResponseOk<UserConsentURL>, HttpError> {
    sentry::start_session();

    // Initialize the MailChimp client.
    let g = MailChimp::new_from_env("", "", "");

    sentry::end_session();
    Ok(HttpResponseOk(UserConsentURL {
        url: g.user_consent_url(),
    }))
}

/** Listen for callbacks to MailChimp auth. */
#[endpoint {
    method = GET,
    path = "/auth/mailchimp/callback",
}]
async fn listen_auth_mailchimp_callback(
    rqctx: Arc<RequestContext<Context>>,
    query_args: Query<AuthCallback>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers_auth::handle_auth_mailchimp_callback(rqctx, query_args).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/** Get the consent URL for Gusto auth. */
#[endpoint {
    method = GET,
    path = "/auth/gusto/consent",
}]
async fn listen_auth_gusto_consent(
    _rqctx: Arc<RequestContext<Context>>,
) -> Result<HttpResponseOk<UserConsentURL>, HttpError> {
    sentry::start_session();

    // Initialize the Gusto client.
    let g = Gusto::new_from_env("", "");

    sentry::end_session();
    Ok(HttpResponseOk(UserConsentURL {
        // We don't need to define scopes for Gusto.
        url: g.user_consent_url(&[]),
    }))
}

/** Listen for callbacks to Gusto auth. */
#[endpoint {
    method = GET,
    path = "/auth/gusto/callback",
}]
async fn listen_auth_gusto_callback(
    rqctx: Arc<RequestContext<Context>>,
    query_args: Query<AuthCallback>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers_auth::handle_auth_gusto_callback(rqctx, query_args).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/** Listen to deauthorization requests for our Zoom app. */
#[endpoint {
    method = GET,
    path = "/auth/zoom/deauthorization",
}]
async fn listen_auth_zoom_deauthorization(
    _rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<serde_json::Value>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    let event = body_param.into_inner();

    warn!("zoom deauthorization: {:?}", event);

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/** Get the consent URL for Zoom auth. */
#[endpoint {
    method = GET,
    path = "/auth/zoom/consent",
}]
async fn listen_auth_zoom_consent(
    _rqctx: Arc<RequestContext<Context>>,
) -> Result<HttpResponseOk<UserConsentURL>, HttpError> {
    sentry::start_session();

    // Initialize the Zoom client.
    let g = Zoom::new_from_env("", "");

    sentry::end_session();
    Ok(HttpResponseOk(UserConsentURL {
        url: g.user_consent_url(&[]),
    }))
}

/** Listen for callbacks to Zoom auth. */
#[endpoint {
    method = GET,
    path = "/auth/zoom/callback",
}]
async fn listen_auth_zoom_callback(
    rqctx: Arc<RequestContext<Context>>,
    query_args: Query<AuthCallback>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers_auth::handle_auth_zoom_callback(rqctx, query_args).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/** Get the consent URL for Ramp auth. */
#[endpoint {
    method = GET,
    path = "/auth/ramp/consent",
}]
async fn listen_auth_ramp_consent(
    _rqctx: Arc<RequestContext<Context>>,
) -> Result<HttpResponseOk<UserConsentURL>, HttpError> {
    sentry::start_session();

    // Initialize the Ramp client.
    let g = Ramp::new_from_env("", "");

    sentry::end_session();
    Ok(HttpResponseOk(UserConsentURL {
        url: g.user_consent_url(&[
            "transactions:read".to_string(),
            "users:read".to_string(),
            "users:write".to_string(),
            "receipts:read".to_string(),
            "cards:read".to_string(),
            "departments:read".to_string(),
            "reimbursements:read".to_string(),
        ]),
    }))
}

/** Listen for callbacks to Ramp auth. */
#[endpoint {
    method = GET,
    path = "/auth/ramp/callback",
}]
async fn listen_auth_ramp_callback(
    rqctx: Arc<RequestContext<Context>>,
    query_args: Query<AuthCallback>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers_auth::handle_auth_ramp_callback(rqctx, query_args).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/** Get the consent URL for Slack auth. */
#[endpoint {
    method = GET,
    path = "/auth/slack/consent",
}]
async fn listen_auth_slack_consent(
    _rqctx: Arc<RequestContext<Context>>,
) -> Result<HttpResponseOk<UserConsentURL>, HttpError> {
    sentry::start_session();

    // Initialize the Slack client.
    let s = Slack::new_from_env("", "", "");

    sentry::end_session();
    Ok(HttpResponseOk(UserConsentURL {
        url: s.user_consent_url(),
    }))
}

/** Listen for callbacks to Slack auth. */
#[endpoint {
    method = GET,
    path = "/auth/slack/callback",
}]
async fn listen_auth_slack_callback(
    rqctx: Arc<RequestContext<Context>>,
    query_args: Query<AuthCallback>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers_auth::handle_auth_slack_callback(rqctx, query_args).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/** Get the consent URL for QuickBooks auth. */
#[endpoint {
    method = GET,
    path = "/auth/quickbooks/consent",
}]
async fn listen_auth_quickbooks_consent(
    _rqctx: Arc<RequestContext<Context>>,
) -> Result<HttpResponseOk<UserConsentURL>, HttpError> {
    sentry::start_session();

    // Initialize the QuickBooks client.
    let g = QuickBooks::new_from_env("", "", "");

    sentry::end_session();
    Ok(HttpResponseOk(UserConsentURL {
        url: g.user_consent_url(),
    }))
}

/** Listen for callbacks to QuickBooks auth. */
#[endpoint {
    method = GET,
    path = "/auth/quickbooks/callback",
}]
async fn listen_auth_quickbooks_callback(
    rqctx: Arc<RequestContext<Context>>,
    query_args: Query<AuthCallback>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers_auth::handle_auth_quickbooks_callback(rqctx, query_args).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/** Listen for webhooks from Plaid. */
#[endpoint {
    method = POST,
    path = "/plaid",
}]
async fn listen_auth_plaid_callback(
    _rqctx: Arc<RequestContext<Context>>,
    body_args: TypedBody<serde_json::Value>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();
    let event = body_args.into_inner();

    warn!("plaid callback: {:?}", event);

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/** Get the consent URL for DocuSign auth. */
#[endpoint {
    method = GET,
    path = "/auth/docusign/consent",
}]
async fn listen_auth_docusign_consent(
    _rqctx: Arc<RequestContext<Context>>,
) -> Result<HttpResponseOk<UserConsentURL>, HttpError> {
    sentry::start_session();

    // Initialize the DocuSign client.
    let g = DocuSign::new_from_env("", "", "", "");

    sentry::end_session();
    Ok(HttpResponseOk(UserConsentURL {
        url: g.user_consent_url(),
    }))
}

/** Listen for callbacks to DocuSign auth. */
#[endpoint {
    method = GET,
    path = "/auth/docusign/callback",
}]
async fn listen_auth_docusign_callback(
    rqctx: Arc<RequestContext<Context>>,
    query_args: Query<AuthCallback>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers_auth::handle_auth_docusign_callback(rqctx, query_args).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/** Listen for updates to our docusign envelopes. */
#[endpoint {
    method = POST,
    path = "/docusign/envelope/update",
}]
async fn listen_docusign_envelope_update_webhooks(
    rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<docusign::Envelope>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_docusign_envelope_update(rqctx, body_param).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/** Listen for analytics page view events. */
#[endpoint {
    method = POST,
    path = "/analytics/page_view",
}]
async fn listen_analytics_page_view_webhooks(
    rqctx: Arc<RequestContext<Context>>,
    body_param: TypedBody<NewPageView>,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_analytics_page_view(rqctx, body_param).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/** Ping endpoint for MailChimp mailing list webhooks. */
#[endpoint {
    method = GET,
    path = "/mailchimp/mailing_list",
}]
async fn ping_mailchimp_mailing_list_webhooks(
    _rqctx: Arc<RequestContext<Context>>,
) -> Result<HttpResponseOk<String>, HttpError> {
    Ok(HttpResponseOk("ok".to_string()))
}

/** Listen for MailChimp mailing list webhooks. */
#[endpoint {
    method = POST,
    path = "/mailchimp/mailing_list",
}]
async fn listen_mailchimp_mailing_list_webhooks(
    rqctx: Arc<RequestContext<Context>>,
    body_param: UntypedBody,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_mailchimp_mailing_list(rqctx, body_param).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/** Ping endpoint for MailChimp rack line webhooks. */
#[endpoint {
    method = GET,
    path = "/mailchimp/rack_line",
}]
async fn ping_mailchimp_rack_line_webhooks(
    _rqctx: Arc<RequestContext<Context>>,
) -> Result<HttpResponseOk<String>, HttpError> {
    Ok(HttpResponseOk("ok".to_string()))
}

/** Listen for MailChimp rack line webhooks. */
#[endpoint {
    method = POST,
    path = "/mailchimp/rack_line",
}]
async fn listen_mailchimp_rack_line_webhooks(
    rqctx: Arc<RequestContext<Context>>,
    body_param: UntypedBody,
) -> Result<HttpResponseAccepted<String>, HttpError> {
    sentry::start_session();

    if let Err(e) = crate::handlers::handle_mailchimp_rack_line(rqctx, body_param).await {
        // Send the error to sentry.
        return Err(handle_anyhow_err_as_http_err(e));
    }

    sentry::end_session();
    Ok(HttpResponseAccepted("ok".to_string()))
}

/** Listen for Slack commands webhooks. */
#[endpoint {
    method = POST,
    path = "/slack/commands",
}]
async fn listen_slack_commands_webhooks(
    rqctx: Arc<RequestContext<Context>>,
    body_param: UntypedBody,
) -> Result<HttpResponseOk<serde_json::Value>, HttpError> {
    sentry::start_session();

    match crate::handlers::handle_slack_commands(rqctx, body_param).await {
        Ok(r) => {
            sentry::end_session();
            Ok(HttpResponseOk(r))
        }
        // Send the error to sentry.
        Err(e) => {
            sentry::end_session();
            Err(handle_anyhow_err_as_http_err(e))
        }
    }
}

#[derive(Deserialize, Debug, JsonSchema)]
pub struct FunctionPathParams {
    pub uuid: String,
}

/** Get information about a function by its uuid. */
#[endpoint {
    method = GET,
    path = "/functions/{uuid}",
}]
async fn listen_get_function_by_uuid(
    rqctx: Arc<RequestContext<Context>>,
    path_params: Path<FunctionPathParams>,
) -> Result<HttpResponseOk<Function>, HttpError> {
    sentry::start_session();

    match crate::handlers_cron::handle_get_function_by_uuid(rqctx, path_params).await {
        Ok(r) => {
            sentry::end_session();
            Ok(HttpResponseOk(r))
        }
        // Send the error to sentry.
        Err(e) => {
            sentry::end_session();
            Err(handle_anyhow_err_as_http_err(e))
        }
    }
}

/** Get a functions logs by its uuid. */
#[endpoint {
    method = GET,
    path = "/functions/{uuid}/logs",
}]
async fn listen_get_function_logs_by_uuid(
    rqctx: Arc<RequestContext<Context>>,
    path_params: Path<FunctionPathParams>,
) -> Result<HttpResponseOk<String>, HttpError> {
    sentry::start_session();

    match crate::handlers_cron::handle_get_function_logs_by_uuid(rqctx, path_params).await {
        Ok(r) => {
            sentry::end_session();
            Ok(HttpResponseOk(r))
        }
        // Send the error to sentry.
        Err(e) => {
            sentry::end_session();
            Err(handle_anyhow_err_as_http_err(e))
        }
    }
}

/** Listen for triggering a function run of sync repos. */
#[endpoint {
    method = POST,
    path = "/run/sync-repos",
}]
async fn trigger_sync_repos_create(
    rqctx: Arc<RequestContext<Context>>,
) -> Result<HttpResponseAccepted<uuid::Uuid>, HttpError> {
    sentry::start_session();

    match crate::handlers_cron::handle_reexec_cmd(rqctx.context(), "sync-repos").await {
        Ok(r) => {
            sentry::end_session();
            Ok(HttpResponseAccepted(r))
        }
        // Send the error to sentry.
        Err(e) => {
            sentry::end_session();
            Err(handle_anyhow_err_as_http_err(e))
        }
    }
}

/** Listen for triggering a function run of sync RFDs. */
#[endpoint {
    method = POST,
    path = "/run/sync-rfds",
}]
async fn trigger_sync_rfds_create(
    rqctx: Arc<RequestContext<Context>>,
) -> Result<HttpResponseAccepted<uuid::Uuid>, HttpError> {
    sentry::start_session();

    match crate::handlers_cron::handle_reexec_cmd(rqctx.context(), "sync-rfds").await {
        Ok(r) => {
            sentry::end_session();
            Ok(HttpResponseAccepted(r))
        }
        // Send the error to sentry.
        Err(e) => {
            sentry::end_session();
            Err(handle_anyhow_err_as_http_err(e))
        }
    }
}

/** Listen for triggering a function run of sync travel. */
#[endpoint {
    method = POST,
    path = "/run/sync-travel",
}]
async fn trigger_sync_travel_create(
    rqctx: Arc<RequestContext<Context>>,
) -> Result<HttpResponseAccepted<uuid::Uuid>, HttpError> {
    sentry::start_session();

    match crate::handlers_cron::handle_reexec_cmd(rqctx.context(), "sync-travel").await {
        Ok(r) => {
            sentry::end_session();
            Ok(HttpResponseAccepted(r))
        }
        // Send the error to sentry.
        Err(e) => {
            sentry::end_session();
            Err(handle_anyhow_err_as_http_err(e))
        }
    }
}

/** Listen for triggering a function run of sync finance. */
#[endpoint {
    method = POST,
    path = "/run/sync-finance",
}]
async fn trigger_sync_finance_create(
    rqctx: Arc<RequestContext<Context>>,
) -> Result<HttpResponseAccepted<uuid::Uuid>, HttpError> {
    sentry::start_session();

    match crate::handlers_cron::handle_reexec_cmd(rqctx.context(), "sync-finance").await {
        Ok(r) => {
            sentry::end_session();
            Ok(HttpResponseAccepted(r))
        }
        // Send the error to sentry.
        Err(e) => {
            sentry::end_session();
            Err(handle_anyhow_err_as_http_err(e))
        }
    }
}

/** Listen for triggering a function run of sync shipments. */
#[endpoint {
    method = POST,
    path = "/run/sync-shipments",
}]
async fn trigger_sync_shipments_create(
    rqctx: Arc<RequestContext<Context>>,
) -> Result<HttpResponseAccepted<uuid::Uuid>, HttpError> {
    sentry::start_session();

    match crate::handlers_cron::handle_reexec_cmd(rqctx.context(), "sync-shipments").await {
        Ok(r) => {
            sentry::end_session();
            Ok(HttpResponseAccepted(r))
        }
        // Send the error to sentry.
        Err(e) => {
            sentry::end_session();
            Err(handle_anyhow_err_as_http_err(e))
        }
    }
}

fn handle_anyhow_err_as_http_err(err: anyhow::Error) -> HttpError {
    // Send to sentry.
    sentry_anyhow::capture_anyhow(&err);
    sentry::end_session();

    // We use the debug formatting here so we get the stack trace.
    return HttpError::for_internal_error(format!("{:?}", err));
}