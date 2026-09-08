//! Private announcement video stages, media tickets and validation.
use kabipay_common::{
    KabiPayError,KabiPayResult
};
use chrono::{
    DateTime,Duration,Utc
};
use uuid::Uuid;
use sea_orm::{
    ActiveModelTrait,ColumnTrait,ConnectionTrait,DatabaseConnection,EntityTrait,QueryFilter,QuerySelect,Set,TransactionTrait
};
use kabipay_db_entities::tenant::{
    d0079_announcement_video::announcement_video_stage as stage,d0029_file_storage::file_storage,d0027_communication_audit::announcement
};
use super::{
    announcement_audience,object_store::{
        FileStorageMode,S3CompatSettings,tenant_bucket_name
    }
};
use base64::{
    Engine,engine::general_purpose::URL_SAFE_NO_PAD
};
use hmac::{
    Hmac,Mac
};
use sha2::Sha256;
pub const MAX_VIDEO_BYTES:u64=50*1024*1024;
fn invalid(message:&str)->KabiPayError{
    KabiPayError::Validation(message.into())
}
pub fn validate_size(size:u64)->KabiPayResult<()>{
    if size==0 || size>MAX_VIDEO_BYTES {
        Err(invalid("video must be between 1 byte and 50 MiB"))
    } else {
        Ok(())
    }
}
pub fn validate_signature(mime:&str,bytes:&[u8])->KabiPayResult<()>{
    let valid=match mime {
        "video/mp4"=>bytes.len()>=12 && &bytes[4..8]==b"ftyp" && u32::from_be_bytes(bytes[..4].try_into().map_err(|_|invalid("invalid video"))?)>=12,
        "video/webm"=>bytes.starts_with(b"\x1a\x45\xdf\xa3") && bytes.windows(7).any(|w|w==b"\x42\x82\x84webm"),_=>false
    };
    if valid {
        Ok(())
    }else{
        Err(invalid("video content must match its declared MP4 or WebM container"))
    }
}
pub fn validate_link(value:&str)->KabiPayResult<String>{
    let value=value.trim();
    if value.len()>4096 || value.chars().any(char::is_control){
        return Err(invalid("invalid video URL"));
    }
    let url=reqwest::Url::parse(value).map_err(|_|invalid("enter a valid HTTP(S) video URL"))?;
    if !matches!(url.scheme(),"http"|"https") || url.host_str().is_none() || !url.username().is_empty() || url.password().is_some(){
        return Err(invalid("enter a valid HTTP(S) video URL without credentials"));
    } Ok(url.to_string())
}
pub fn byte_range(value:Option<&str>,size:u64)->Result<(u64,u64,bool),()>{
    if size==0{
        return Err(());
    } let Some(value)=value else{
        return Ok((0,size-1,false));
    };
    let (start,end)=value.strip_prefix("bytes=").ok_or(())?.split_once('-').ok_or(())?;
    if !start.bytes().all(|b|b.is_ascii_digit()) || !end.bytes().all(|b|b.is_ascii_digit()){
        return Err(());
    }
    if start.is_empty(){
        let suffix=end.parse::<u64>().map_err(|_|())?;
        if suffix==0{
            return Err(());
        }return Ok((size.saturating_sub(suffix),size-1,true));
    }
    let start=start.parse::<u64>().map_err(|_|())?;
    let end=if end.is_empty(){
        size-1
    }else{
        end.parse::<u64>().map_err(|_|())?.min(size-1)
    };
    if start>=size || end<start {
        return Err(());
    } Ok((start,end,true))
}
#[derive(Clone,Debug,serde::Serialize,serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaTicket{
    pub tenant:Uuid,pub user:Uuid,pub resource:Uuid,pub purpose:String,pub exp:i64
}
pub fn sign_ticket(ticket:&MediaTicket)->KabiPayResult<String>{
    let payload=serde_json::to_vec(ticket).map_err(|_|invalid("invalid media ticket"))?;
    let mut mac=Hmac::<Sha256>::new_from_slice(&kabipay_common::jwt::jwt_secret_from_env()).map_err(|_|invalid("invalid media key"))?;
    mac.update(&payload);
    Ok(format!("{}.{}",URL_SAFE_NO_PAD.encode(payload),URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())))
}
pub fn verify_ticket(token:&str,purpose:&str)->KabiPayResult<MediaTicket>{
    let verify=||->Option<MediaTicket>{
        if token.len()>2048{
            return None;
        }let (p,s)=token.split_once('.')?;
        let payload=URL_SAFE_NO_PAD.decode(p).ok()?;
        let signature=URL_SAFE_NO_PAD.decode(s).ok()?;
        let mut mac=Hmac::<Sha256>::new_from_slice(&kabipay_common::jwt::jwt_secret_from_env()).ok()?;
        mac.update(&payload);
        mac.verify_slice(&signature).ok()?;
        let ticket:MediaTicket=serde_json::from_slice(&payload).ok()?;
        if ticket.purpose!=purpose || ticket.exp<=Utc::now().timestamp(){
            return None;
        }Some(ticket)
    };
    verify().ok_or(KabiPayError::Unauthorised)
}
fn media_url(ticket:&MediaTicket,action:&str)->KabiPayResult<String>{
    let base=std::env::var("KABIPAY_NOTIFICATION_PUBLIC_BASE").map_err(|_|invalid("KABIPAY_NOTIFICATION_PUBLIC_BASE must be configured"))?;
    let base=validate_link(&base)?;
    let mut url=reqwest::Url::parse(&base).map_err(|_|invalid("invalid public base"))?;
    if url.query().is_some() || url.fragment().is_some() || url.path()!="/" {
        return Err(invalid("notification public base must be an HTTP(S) origin"));
    }
    if !cfg!(debug_assertions) && (url.scheme()!="https" || matches!(url.host_str(),Some("localhost"|"127.0.0.1"|"0.0.0.0"|"[::1]"))){
        return Err(invalid("notification public base must be public HTTPS"));
    }
    url.set_path(&format!("/files/announcement-video/{action}"));
    url.query_pairs_mut().append_pair("token",&sign_ticket(ticket)?);
    Ok(url.into())
}
#[derive(async_graphql::SimpleObject)]
pub struct AnnouncementVideoUpload{
    pub stage_id:Uuid,pub upload_url:String,pub expires_at:DateTime<Utc>
}
#[derive(async_graphql::SimpleObject)]
pub struct AnnouncementVideo{
    pub playback_url:String,pub mime_type:String,pub file_name:String,pub expires_at:DateTime<Utc>
}
pub async fn prepare(db:&DatabaseConnection,tenant:Uuid,user:Uuid,name:String,mime:String,size:i32)->KabiPayResult<AnnouncementVideoUpload>{
    validate_size(u64::try_from(size).map_err(|_|invalid("invalid video size"))?)?;
    if !matches!(mime.as_str(),"video/mp4"|"video/webm") {
        return Err(invalid("choose MP4 or WebM"));
    }
    if name.trim().is_empty() || name.len()>255 || name.chars().any(char::is_control){
        return Err(invalid("invalid video filename"));
    }
    announcement_audience::current_viewer(db,tenant,user).await?;
    let id=Uuid::new_v4();
    let file_id=Uuid::new_v4();
    let now=Utc::now();
    let expiry=now+Duration::minutes(15);
    let ticket=MediaTicket{
        tenant,user,resource:id,purpose:"announcement-video-upload".into(),exp:expiry.timestamp()
    };
    let url=media_url(&ticket,"upload")?;
    let (provider,bucket)=match FileStorageMode::from_env(){
        FileStorageMode::Local=>("LOCAL",None),FileStorageMode::S3Compat=>{
            let cfg=S3CompatSettings::from_env()?;
            let bucket=if cfg.per_tenant_bucket{
                tenant_bucket_name(tenant,&cfg.bucket_prefix)
            }else{
                cfg.default_bucket.ok_or_else(||invalid("missing storage bucket"))?
            };
            ("S3",Some(bucket))
        },FileStorageMode::AzureBlob=>return Err(invalid("video storage provider is unsupported"))
    };
    let tx=db.begin().await?;
    file_storage::ActiveModel{
        id:Set(file_id),tenant_id:Set(tenant),provider:Set(provider.into()),bucket:Set(bucket),storage_path:Set(format!("tenants/{tenant}/announcement-video/{file_id}")),original_filename:Set(Some(name)),mime_type:Set(Some(mime)),file_size_bytes:Set(Some(i64::from(size))),is_public:Set(false),uploaded_by:Set(Some(user)),created_at:Set(now),updated_at:Set(now)
    }.insert(&tx).await?;
    stage::ActiveModel{
        id:Set(id),tenant_id:Set(tenant),created_by:Set(user),file_storage_id:Set(file_id),purpose:Set("ANNOUNCEMENT_VIDEO".into()),uploaded_at:Set(None),claimed_announcement_id:Set(None),expires_at:Set(expiry),created_at:Set(now)
    }.insert(&tx).await?;
    tx.commit().await?;
    Ok(AnnouncementVideoUpload{
        stage_id:id,upload_url:url,expires_at:expiry
    })
}
pub fn claimable(row:&stage::Model,tenant:Uuid,user:Uuid)->bool{
    row.tenant_id==tenant && row.created_by==user && row.purpose=="ANNOUNCEMENT_VIDEO" && row.uploaded_at.is_some() && row.claimed_announcement_id.is_none() && row.expires_at>Utc::now()
}
pub async fn claim(db:&impl ConnectionTrait,tenant:Uuid,user:Uuid,id:Uuid,parent:Uuid)->KabiPayResult<Uuid>{
    let row=stage::Entity::find_by_id(id).filter(stage::Column::TenantId.eq(tenant)).lock_exclusive().one(db).await?.filter(|r|claimable(r,tenant,user)).ok_or_else(||invalid("video upload has expired or is unavailable; upload it again"))?;
    let file=row.file_storage_id;
    let mut am:stage::ActiveModel=row.into();
    am.claimed_announcement_id=Set(Some(parent));
    am.update(db).await?;
    Ok(file)
}
pub async fn playback(db:&DatabaseConnection,tenant:Uuid,user:Uuid,id:Uuid)->KabiPayResult<AnnouncementVideo>{
    let parent=announcement_audience::authorized_parent(db,tenant,user,id).await?;
    let file=video_file(db,tenant,&parent).await?;
    let expires=Utc::now()+Duration::minutes(5);
    let ticket=MediaTicket{
        tenant,user,resource:id,purpose:"announcement-video-play".into(),exp:expires.timestamp()
    };
    Ok(AnnouncementVideo{
        playback_url:media_url(&ticket,"play")?,mime_type:file.mime_type.unwrap_or_default(),file_name:file.original_filename.unwrap_or_else(||"Video".into()),expires_at:expires
    })
}
pub async fn video_file(db:&DatabaseConnection,tenant:Uuid,parent:&announcement::Model)->KabiPayResult<file_storage::Model>{
    let id=parent.video_file_storage_id.ok_or_else(||invalid("announcement has no uploaded video"))?;
    file_storage::Entity::find_by_id(id).filter(file_storage::Column::TenantId.eq(tenant)).filter(file_storage::Column::IsPublic.eq(false)).one(db).await?.ok_or_else(||invalid("video is unavailable"))
}
pub async fn sweep_expired(db:&DatabaseConnection,tenant:Uuid,limit:u64)->KabiPayResult<()>{
    let tx=db.begin().await?;
    let rows=stage::Entity::find().filter(stage::Column::TenantId.eq(tenant)).filter(stage::Column::ClaimedAnnouncementId.is_null()).filter(stage::Column::ExpiresAt.lte(Utc::now()-Duration::minutes(10))).limit(limit.min(100)).lock_with_behavior(sea_orm::sea_query::LockType::Update,sea_orm::sea_query::LockBehavior::SkipLocked).all(&tx).await?;
    for row in rows{
        if announcement::Entity::find().filter(announcement::Column::TenantId.eq(tenant)).filter(sea_orm::Condition::any().add(announcement::Column::VideoFileStorageId.eq(row.file_storage_id)).add(announcement::Column::ImageFileStorageId.eq(row.file_storage_id)).add(announcement::Column::DocumentFileStorageId.eq(row.file_storage_id))).one(&tx).await?.is_some(){
            continue;
        }
        stage::Entity::delete_by_id(row.id).exec(&tx).await?;
        if let Some(file)=file_storage::Entity::find_by_id(row.file_storage_id).filter(file_storage::Column::TenantId.eq(tenant)).one(&tx).await?{
            if file.provider=="S3"{
                enqueue_spool_cleanup(&tx,tenant,&file).await?;
            }kabipay_common::private_file_cleanup::enqueue_and_delete_private_file(&tx,tenant,&file).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}
#[cfg(test)]
mod security_tests {
    use super::*;
    #[test] fn ticket_rejects_wrong_purpose_expiry_and_tampering(){
        let mut ticket=MediaTicket{
            tenant:Uuid::new_v4(),user:Uuid::new_v4(),resource:Uuid::new_v4(),purpose:"announcement-video-play".into(),exp:Utc::now().timestamp()+60
        };
        let token=sign_ticket(&ticket).unwrap();
        assert!(verify_ticket(&token,"announcement-video-play").is_ok());
        assert!(verify_ticket(&token,"announcement-video-upload").is_err());
        assert!(verify_ticket(&(token+"x"),"announcement-video-play").is_err());
        ticket.exp=Utc::now().timestamp();
        assert!(verify_ticket(&sign_ticket(&ticket).unwrap(),"announcement-video-play").is_err());
    }
    #[test] fn stage_rejects_wrong_owner_reuse_expiry_and_purpose(){
        let tenant=Uuid::new_v4();
        let owner=Uuid::new_v4();
        let mut row=stage::Model{
            id:Uuid::new_v4(),tenant_id:tenant,created_by:owner,file_storage_id:Uuid::new_v4(),purpose:"ANNOUNCEMENT_VIDEO".into(),uploaded_at:Some(Utc::now()),claimed_announcement_id:None,expires_at:Utc::now()+Duration::minutes(1),created_at:Utc::now()
        };
        assert!(claimable(&row,tenant,owner));
        assert!(!claimable(&row,Uuid::new_v4(),owner));
        assert!(!claimable(&row,tenant,Uuid::new_v4()));
        row.claimed_announcement_id=Some(Uuid::new_v4());
        assert!(!claimable(&row,tenant,owner));
        row.claimed_announcement_id=None;
        row.purpose="COMPANY_DOCUMENT".into();
        assert!(!claimable(&row,tenant,owner));
        row.purpose="ANNOUNCEMENT_VIDEO".into();
        row.expires_at=Utc::now();
        assert!(!claimable(&row,tenant,owner));
        row.expires_at=Utc::now()+Duration::minutes(1);
        row.uploaded_at=None;
        assert!(!claimable(&row,tenant,owner));
    }
}
pub fn spool_path(file:&file_storage::Model)->String {
    format!("{}.upload",file.storage_path)
}
pub async fn enqueue_spool_cleanup(db:&impl ConnectionTrait,tenant:Uuid,file:&file_storage::Model)->KabiPayResult<()> {
    kabipay_common::private_file_cleanup::enqueue_private_file_cleanup_coordinates(db,tenant,"LOCAL",None,&spool_path(file)).await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn size_bounds() {
        assert!(validate_size(0).is_err());
        assert!(validate_size(MAX_VIDEO_BYTES+1).is_err());
        assert!(validate_size(MAX_VIDEO_BYTES).is_ok());
    }
    #[test] fn signatures() {
        assert!(validate_signature("video/mp4", b"\0\0\0\x18ftypisom").is_ok());
        assert!(validate_signature("video/webm", b"\x1a\x45\xdf\xa3\x42\x82\x84webm").is_ok());
        assert!(validate_signature("video/mp4", b"\x1a\x45\xdf\xa3webm").is_err());
        assert!(validate_signature("video/webm", b"\x1a\x45\xdf\xa3matroska").is_err());
    }
    #[test] fn links() {
        for value in ["javascript:alert(1)","data:text/html,hello","https://user:pass@example.com/a", "https://"] {
            assert!(validate_link(value).is_err(), "{value}");
        } assert_eq!(validate_link(" https://example.com/watch?v=1 ").unwrap(),"https://example.com/watch?v=1");
    }
    #[test] fn ranges() {
        assert_eq!(byte_range(Some("bytes=10-19"),100),Ok((10,19,true)));
        assert_eq!(byte_range(Some("bytes=-10"),100),Ok((90,99,true)));
        assert_eq!(byte_range(Some("bytes=90-"),100),Ok((90,99,true)));
        for value in ["bytes=100-", "bytes=20-10", "bytes=0-1,5-9", "bytes=-0"] {
            assert!(byte_range(Some(value),100).is_err());
        }
    }
}
#[cfg(test)]
mod contract_tests {
    use async_graphql::{
        Schema, EmptySubscription
    };
    use crate::resolvers::{
        QueryRoot, MutationRoot
    };
    #[test] fn media_contract_exists() {
        let sdl=Schema::build(QueryRoot,MutationRoot,EmptySubscription).finish().sdl();
        for field in ["prepareAnnouncementVideoUpload(", "announcementVideo(","videoUploadStageId:","hasVideoAttachment:", "removeVideo:"] {
            assert!(sdl.contains(field),"{field}");
        }
    }
}

/// Media tickets also require a currently active control-plane tenant mapping.
pub async fn required_db(ctx:&async_graphql::Context<'_>,tenant:Uuid)->async_graphql::Result<DatabaseConnection> {
    kabipay_common::db::resolve_required_tenant_db(
        tenant,
        ctx.data::<DatabaseConnection>()?,
        ctx.data::<kabipay_common::db::TenantDbCache>()?,
        ctx.data::<kabipay_common::db::TenantDbConfig>()?,
    ).await.map_err(KabiPayError::into_graphql)
}
