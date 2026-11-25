//! Fixed category text and authenticated feature links, never business payloads,
//! attachment URLs, token links or tenant-supplied HTML templates.
use super::settings_types::valid_mailbox;
use kabipay_common::important_email::*;
use lettre::{
    message::{header::ContentType, Mailbox},
    Address, Message,
};
use reqwest::Url;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemplateError {
    InvalidSender,
    InvalidCompany,
    InvalidOrigin,
    InvalidEvent,
    InvalidRecipient,
    MessageBuild,
}
pub struct EmailSender {
    name: String,
    address: Address,
    account: Address,
}
impl EmailSender {
    pub fn new(name: String, address: String, account: String) -> Result<Self, TemplateError> {
        if name.chars().count() > 120
            || name.chars().any(char::is_control)
            || !valid_mailbox(&address)
            || !valid_mailbox(&account)
        {
            return Err(TemplateError::InvalidSender);
        }
        Ok(Self {
            name,
            address: address.parse().map_err(|_| TemplateError::InvalidSender)?,
            account: account.parse().map_err(|_| TemplateError::InvalidSender)?,
        })
    }
}
impl std::fmt::Debug for EmailSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EmailSender([redacted])")
    }
}
pub struct RenderedEmail {
    sender: Address,
    recipient: Address,
    bytes: Vec<u8>,
    message_id: String,
    text: String,
    link: String,
}
impl RenderedEmail {
    pub fn sender(&self) -> &Address {
        &self.sender
    }
    pub fn recipient(&self) -> &Address {
        &self.recipient
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn message_id(&self) -> &str {
        &self.message_id
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn link(&self) -> &str {
        &self.link
    }
}
impl std::fmt::Debug for RenderedEmail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RenderedEmail([redacted])")
    }
}
fn origin(value: &str) -> Result<Url, TemplateError> {
    let url = Url::parse(value).map_err(|_| TemplateError::InvalidOrigin)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(TemplateError::InvalidOrigin);
    }
    Ok(url)
}
fn company(value: &str) -> Result<(), TemplateError> {
    if value.trim().is_empty() || value.chars().count() > 160 || value.chars().any(char::is_control)
    {
        return Err(TemplateError::InvalidCompany);
    }
    Ok(())
}
pub fn render_saved_account_test(
    event: &EmailEvent,
    company_name: &str,
    company_origin: &str,
    sender: &EmailSender,
) -> Result<RenderedEmail, TemplateError> {
    event.validate().map_err(|_| TemplateError::InvalidEvent)?;
    company(company_name)?;
    let owning = origin(company_origin)?;
    if event.kind != EmailEventKind::Test
        || !matches!(
            event.action_target,
            EmailActionTarget::CompanyResource {
                resource_type: EmailResourceType::Test,
                ..
            }
        )
    {
        return Err(TemplateError::InvalidEvent);
    }
    let EmailRecipientRef::LocalUser { user_id } = event.recipient else {
        return Err(TemplateError::InvalidEvent);
    };
    build(event,company_name,&owning,sender,sender.account.clone(),EmailRecipientIdentity{source_tenant_id:event.tenant_id,source_user_id:user_id},"Email settings test",owning.to_string(),"This is a test of this company's email settings. SMTP acceptance does not confirm inbox delivery.")
}
pub fn render_email(
    event: &EmailEvent,
    company_name: &str,
    company_origin: &str,
    sender: &EmailSender,
    recipient: &PrivateEmailRecipient,
) -> Result<RenderedEmail, TemplateError> {
    event.validate().map_err(|_| TemplateError::InvalidEvent)?;
    company(company_name)?;
    let owning = origin(company_origin)?;
    if event.kind == EmailEventKind::Test {
        return Err(TemplateError::InvalidEvent);
    }
    if !valid_mailbox(recipient.address()) {
        return Err(TemplateError::InvalidRecipient);
    }
    let address = recipient
        .address()
        .parse()
        .map_err(|_| TemplateError::InvalidRecipient)?;
    let mut link = match event.action_target {
        EmailActionTarget::OrganizationResource {
            organization_id,
            membership_id,
            ..
        } => {
            let EmailRecipientRef::OrganizationPrincipal {
                source_tenant_id, ..
            } = event.recipient
            else {
                return Err(TemplateError::InvalidRecipient);
            };
            if recipient.identity.source_tenant_id != source_tenant_id {
                return Err(TemplateError::InvalidRecipient);
            }
            let mut url = origin(recipient.source_login_origin())?;
            url.set_path("/organization-workspace");
            url.query_pairs_mut()
                .append_pair("organization", &organization_id.to_string())
                .append_pair("company", &membership_id.to_string())
                .append_pair("section", "inbox");
            url
        }
        EmailActionTarget::CompanyResource { .. } => {
            if recipient.identity.source_tenant_id != event.tenant_id {
                return Err(TemplateError::InvalidRecipient);
            }
            if !matches!(event.recipient,EmailRecipientRef::LocalUser{user_id}if user_id==recipient.identity.source_user_id)
            {
                return Err(TemplateError::InvalidRecipient);
            }
            let mut url = owning.clone();
            let action = event.kind == EmailEventKind::ActionRequired;
            url.set_path(match event.resource_type {
                EmailResourceType::Leave => {
                    if action {
                        "/hr/leaves"
                    } else {
                        "/leave"
                    }
                }
                EmailResourceType::Expense | EmailResourceType::Travel => "/expenses",
                EmailResourceType::TaxProof => {
                    if action {
                        "/payroll/tax"
                    } else {
                        "/payroll/payslips"
                    }
                }
                EmailResourceType::Timesheet => {
                    if action {
                        "/hr/timesheets"
                    } else {
                        "/timesheet"
                    }
                }
                EmailResourceType::ProfileChange => {
                    if action {
                        "/organization/profile-reviews"
                    } else {
                        "/profile/settings"
                    }
                }
                EmailResourceType::LoanRequest => {
                    if action {
                        "/loans/admin"
                    } else {
                        "/loans/my"
                    }
                }
                EmailResourceType::PerformanceReview => "/performance",
                EmailResourceType::Payslip => "/payroll/payslips",
                EmailResourceType::Attendance => "/attendance",
                EmailResourceType::Test => return Err(TemplateError::InvalidEvent),
            });
            match event.resource_type {
                EmailResourceType::Travel => {
                    url.query_pairs_mut().append_pair("tab", "travel");
                }
                EmailResourceType::Payslip => {
                    url.query_pairs_mut().append_pair("tab", "payslip");
                }
                EmailResourceType::TaxProof if !action => {
                    url.query_pairs_mut().append_pair("tab", "incometax");
                }
                EmailResourceType::PerformanceReview => {
                    url.query_pairs_mut()
                        .append_pair("tab", if action { "team" } else { "my" });
                }
                _ => {}
            };
            url
        }
    };
    link.query_pairs_mut()
        .append_pair("resource", &event.resource_id.to_string())
        .append_pair("resourceType", event.resource_type.as_str());
    let (subject, description) = match (event.kind, event.category) {
        (EmailEventKind::ActionRequired, EmailCategory::Leave) => (
            "Leave approval needed",
            "A leave request needs your review.",
        ),
        (EmailEventKind::ActionRequired, _) => (
            "Approval needed",
            "An approval or review needs your attention.",
        ),
        (EmailEventKind::Outcome, EmailCategory::Leave) => (
            "Leave update",
            "A decision or update is available for your leave request.",
        ),
        (EmailEventKind::Outcome, _) => (
            "Request update",
            "A decision or update is available for your request.",
        ),
        (EmailEventKind::PayslipAvailable, _) => (
            "Payslip available",
            "Your payslip is available in the application.",
        ),
        (EmailEventKind::MissedPunchOut, _) => (
            "Attendance review needed",
            "Please review your attendance and complete any required correction.",
        ),
        _ => return Err(TemplateError::InvalidEvent),
    };
    build(
        event,
        company_name,
        &owning,
        sender,
        address,
        recipient.identity,
        subject,
        link.into(),
        description,
    )
}
#[allow(clippy::too_many_arguments)]
fn build(
    event: &EmailEvent,
    company_name: &str,
    owning: &Url,
    sender: &EmailSender,
    recipient: Address,
    identity: EmailRecipientIdentity,
    subject: &str,
    link: String,
    description: &str,
) -> Result<RenderedEmail, TemplateError> {
    let mut hash = Sha256::new();
    for part in [
        "important-email-message-v1",
        &event.tenant_id.to_string(),
        &event.event_id.to_string(),
        &identity.source_tenant_id.to_string(),
        &identity.source_user_id.to_string(),
    ] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    let message_id = format!(
        "<hrms-{:x}@{}>",
        hash.finalize(),
        owning.host_str().ok_or(TemplateError::InvalidOrigin)?
    );
    let text=format!("{company_name}\n\n{description}\n\nOpen the application and sign in to review the details. Access is checked in the application.\n{link}\n");
    let message = Message::builder()
        .from(Mailbox::new(
            if sender.name.is_empty() {
                None
            } else {
                Some(sender.name.clone())
            },
            sender.address.clone(),
        ))
        .to(Mailbox::new(None, recipient.clone()))
        .subject(format!("{company_name}: {subject}"))
        .message_id(Some(message_id.clone()))
        .date(event.occurred_at.into())
        .header(ContentType::TEXT_PLAIN)
        .body(text.clone())
        .map_err(|_| TemplateError::MessageBuild)?;
    Ok(RenderedEmail {
        sender: sender.address.clone(),
        recipient,
        bytes: message.formatted(),
        message_id,
        text,
        link,
    })
}
