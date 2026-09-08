//! Purpose-bound private announcement media. Storage is accessed only after live authorization.
use std::sync::Arc;
use axum::{
    body::Body,extract::{
        Query,State
    },http::{
        HeaderMap,StatusCode,header
    },response::{
        Response,IntoResponse
    }
};
use futures_util::StreamExt;
use sha2::Digest;
use tokio::io::{
    AsyncReadExt,AsyncWriteExt,AsyncSeekExt
};
use sea_orm::{
    ColumnTrait,EntityTrait,QueryFilter,QuerySelect,ActiveModelTrait,Set,TransactionTrait
};
use kabipay_common::{
    KabiPayError,KabiPayResult,db::{
        resolve_required_tenant_db,TenantDbCache,TenantDbConfig
    }
};
use kabipay_db_entities::tenant::{
    d0079_announcement_video::announcement_video_stage as stage,d0029_file_storage::file_storage
};
use crate::services::{
    announcement_video as video,announcement_audience,announcement_storage,object_store::{
        S3CompatSettings,s3_operator_for_bucket,ensure_tenant_bucket
    }
};
#[derive(Clone)]
pub struct MediaState{
    pub ops:sea_orm::DatabaseConnection,pub cache:TenantDbCache,pub fallback:TenantDbConfig
}
#[derive(serde::Deserialize)]
pub struct TokenQuery{
    token:String
}
fn storage_error(_:impl std::fmt::Display)->KabiPayError{
    KabiPayError::Internal("video storage operation failed; please retry".into())
}
fn bad(message:&str)->KabiPayError{
    KabiPayError::Validation(message.into())
}
fn secure(mut response:Response)->Response{
    let headers=response.headers_mut();
    headers.insert(header::CACHE_CONTROL,"private, no-store".parse().expect("static header"));
    headers.insert("referrer-policy","no-referrer".parse().expect("static header"));
    headers.insert("x-content-type-options","nosniff".parse().expect("static header"));
    response
}
pub async fn upload(State(state):State<Arc<MediaState>>,Query(query):Query<TokenQuery>,headers:HeaderMap,body:Body)->Response{
    let result=tokio::time::timeout(std::time::Duration::from_secs(300),upload_inner(&state,&query.token,headers,body)).await;
    secure(match result{
        Ok(Ok(id))=>axum::Json(serde_json::json!({
            "stageId":id
        })).into_response(),Ok(Err(e))=>e.into_response(),Err(_)=>(StatusCode::REQUEST_TIMEOUT,"Upload timed out; prepare a new upload").into_response()
    })
}
async fn upload_inner(state:&MediaState,token:&str,headers:HeaderMap,body:Body)->KabiPayResult<uuid::Uuid>{
    let ticket=video::verify_ticket(token,"announcement-video-upload")?;
    let db=resolve_required_tenant_db(ticket.tenant,&state.ops,&state.cache,&state.fallback).await?;
    announcement_audience::current_viewer(&db,ticket.tenant,ticket.user).await?;
    let tx=db.begin().await?;
    // Holding this lock until upload completion serializes duplicate sends, claims and expiry cleanup.
    let row=stage::Entity::find_by_id(ticket.resource).filter(stage::Column::TenantId.eq(ticket.tenant)).lock_exclusive().one(&tx).await?.ok_or_else(||bad("upload unavailable"))?;
    if row.created_by!=ticket.user || row.purpose!="ANNOUNCEMENT_VIDEO" || row.uploaded_at.is_some() || row.claimed_announcement_id.is_some() || row.expires_at<=chrono::Utc::now(){
        return Err(bad("upload expired or already used"));
    }
    let file=file_storage::Entity::find_by_id(row.file_storage_id).filter(file_storage::Column::TenantId.eq(ticket.tenant)).one(&tx).await?.ok_or_else(||bad("upload unavailable"))?;
    let expected=u64::try_from(file.file_size_bytes.unwrap_or_default()).map_err(|_|bad("invalid upload size"))?;
    video::validate_size(expected)?;
    let mime=file.mime_type.as_deref().ok_or_else(||bad("missing video type"))?;
    if headers.get(header::CONTENT_TYPE).and_then(|h|h.to_str().ok())!=Some(mime){
        return Err(bad("upload Content-Type must match the prepared video"));
    }
    if let Some(length)=headers.get(header::CONTENT_LENGTH){
        let actual=length.to_str().ok().and_then(|v|v.parse::<u64>().ok()).ok_or_else(||bad("invalid Content-Length"))?;
        if actual!=expected || actual>video::MAX_VIDEO_BYTES{
            return Err(bad("upload size differs from prepared video"));
        }
    }
    let mut sink=Sink::open(&file).await?;
    let streamed=async {
        let digest=write_body(&mut sink,body,expected,mime).await?;
        drop(sink);
        if file.provider=="S3" {
            upload_spool_to_s3(&file,&digest).await?;
        }
        announcement_audience::current_viewer(&db,ticket.tenant,ticket.user).await?;
        if row.expires_at<=chrono::Utc::now(){
            return Err(bad("upload expired"));
        }
        Ok(())
    }.await;
    if let Err(error)=streamed {
        let mut failed:stage::ActiveModel=row.into();
        failed.expires_at=Set(chrono::Utc::now());
        failed.update(&tx).await?;
        tx.commit().await?;
        return Err(error);
    }
    if file.provider=="S3" {
        video::enqueue_spool_cleanup(&tx,ticket.tenant,&file).await?;
    }
    let mut am:stage::ActiveModel=row.into();
    am.uploaded_at=Set(Some(chrono::Utc::now()));
    am.update(&tx).await?;
    tx.commit().await?;
    Ok(ticket.resource)
}
async fn write_body(sink:&mut Sink,body:Body,expected:u64,mime:&str)->KabiPayResult<String>{
    let mut digest=sha2::Sha256::new();
    let mut body=body.into_data_stream();
    let mut length=0u64;
    let mut signature=Vec::with_capacity(4096);
    while let Some(chunk)=body.next().await{
        let chunk=chunk.map_err(storage_error)?;
        length=length.checked_add(chunk.len() as u64).ok_or_else(||bad("video too large"))?;
        if length>expected || length>video::MAX_VIDEO_BYTES{
            return Err(bad("video exceeds declared size or 50 MiB limit"));
        }
        signature.extend_from_slice(&chunk[..chunk.len().min(4096-signature.len())]);
        digest.update(&chunk);
        sink.write(chunk).await?;
    }
    if length!=expected{
        return Err(bad("incomplete upload; retry with a new upload"));
    }
    video::validate_signature(mime,&signature)?;
    sink.finish().await?;
    Ok(format!("{:x}",digest.finalize()))
}
// The stage owns this quarantine file before accepting any bytes. S3 writes use a
// single streaming PUT, so a process crash cannot strand multipart upload parts.
enum Sink {
    Local(tokio::fs::File)
}
impl Sink {
    async fn open(file:&file_storage::Model)->KabiPayResult<Self> {
        let relative=if file.provider=="LOCAL" {
            file.storage_path.clone()
        } else if file.provider=="S3" {
            video::spool_path(file)
        } else {
            return Err(bad("unsupported media provider"));
        };
        let path=announcement_storage::absolute_storage_path(&relative)?;
        tokio::fs::create_dir_all(path.parent().ok_or_else(||bad("invalid storage path"))?).await.map_err(storage_error)?;
        Ok(Self::Local(tokio::fs::File::create(path).await.map_err(storage_error)?))
    }
    async fn write(&mut self,bytes:axum::body::Bytes)->KabiPayResult<()> {
        match self {
            Self::Local(file)=>file.write_all(&bytes).await.map_err(storage_error)
        }
    }
    async fn finish(&mut self)->KabiPayResult<()> {
        match self {
            Self::Local(file)=>file.sync_all().await.map_err(storage_error)
        }
    }
}
async fn upload_spool_to_s3(file:&file_storage::Model,digest:&str)->KabiPayResult<()> {
    let cfg=S3CompatSettings::from_env()?;
    let bucket=file.bucket.as_deref().ok_or_else(||bad("missing bucket"))?;
    ensure_tenant_bucket(&cfg,bucket).await.map_err(storage_error)?;
    let mut url=reqwest::Url::parse(&cfg.endpoint).map_err(storage_error)?;
    let prefix=url.path().trim_end_matches('/').to_owned();
    if cfg.path_style {
        url.set_path(&format!("{prefix}/{bucket}/{}",file.storage_path));
    }
    else {
        let host=url.host_str().ok_or_else(||bad("invalid storage endpoint"))?;
        url.set_host(Some(&format!("{bucket}.{host}"))).map_err(storage_error)?;
        url.set_path(&format!("{prefix}/{}",file.storage_path));
    }
    let source=tokio::fs::File::open(announcement_storage::absolute_storage_path(&video::spool_path(file))?).await.map_err(storage_error)?;
    let stream=futures_util::stream::try_unfold(source,|mut source|async move {
        let mut buffer=vec![0;
        256*1024];
        let count=source.read(&mut buffer).await?;
        if count==0 {
            return Ok::<_,std::io::Error>(None);
        } buffer.truncate(count);
        Ok(Some((buffer,source)))
    });
    let client=reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(std::time::Duration::from_secs(120)).build().map_err(storage_error)?;
    let mut request=client.put(url).header("content-length",file.file_size_bytes.unwrap_or_default()).header("content-type",file.mime_type.as_deref().unwrap_or("application/octet-stream")).header("x-amz-content-sha256",digest).body(reqwest::Body::wrap_stream(stream)).build().map_err(storage_error)?;
    let credentials=reqsign::AwsCredential {
        access_key_id:cfg.access_key_id,secret_access_key:cfg.secret_access_key,session_token:None,expires_in:None
    };
    reqsign::AwsV4Signer::new("s3",&cfg.region).sign(&mut request,&credentials).map_err(storage_error)?;
    let response=client.execute(request).await.map_err(storage_error)?;
    if !response.status().is_success() {
        return Err(storage_error("S3 rejected upload"));
    }
    Ok(())
}
pub async fn play(State(state):State<Arc<MediaState>>,Query(query):Query<TokenQuery>,headers:HeaderMap)->Response{
    secure(match play_inner(&state,&query.token,headers).await{
        Ok(response)=>response,Err(error)=>error.into_response()
    })
}
async fn play_inner(state:&MediaState,token:&str,headers:HeaderMap)->KabiPayResult<Response>{
    let ticket=video::verify_ticket(token,"announcement-video-play")?;
    let db=resolve_required_tenant_db(ticket.tenant,&state.ops,&state.cache,&state.fallback).await?;
    let parent=announcement_audience::authorized_parent(&db,ticket.tenant,ticket.user,ticket.resource).await?;
    let file=video::video_file(&db,ticket.tenant,&parent).await?;
    let size=u64::try_from(file.file_size_bytes.unwrap_or_default()).map_err(|_|bad("invalid video size"))?;
    video::validate_size(size)?;
    let range=match headers.get(header::RANGE){
        Some(header)=>Some(header.to_str().unwrap_or("invalid")),None=>None
    };
    let (start,end,partial)=match video::byte_range(range,size){
        Ok(range)=>range,Err(())=>return Response::builder().status(StatusCode::RANGE_NOT_SATISFIABLE).header(header::CONTENT_RANGE,format!("bytes */{size}")).header(header::ACCEPT_RANGES,"bytes").header(header::CONTENT_LENGTH,"0").body(Body::empty()).map_err(storage_error)
    };
    let mime=file.mime_type.as_deref().filter(|m|matches!(*m,"video/mp4"|"video/webm")).ok_or_else(||bad("unsupported video type"))?.to_owned();
    let reader=Reader::open(&file,start).await?;
    stream_response(reader,mime,size,start,end,partial)
}
fn stream_response(reader:Reader,mime:String,size:u64,start:u64,end:u64,partial:bool)->KabiPayResult<Response>{
    let stream=futures_util::stream::try_unfold((reader,start,end),|(mut reader,position,end)|async move{
        if position>end{
            return Ok(None);
        }let count=(end-position+1).min(256*1024) as usize;
        let bytes=reader.read(position,count).await?;
        if bytes.is_empty(){
            return Err(storage_error("unexpected end"));
        }let next=position+bytes.len() as u64;
        Ok::<_,KabiPayError>(Some((bytes,(reader,next,end))))
    });
    let mut response=Response::builder().status(if partial{
        StatusCode::PARTIAL_CONTENT
    }else{
        StatusCode::OK
    }).header(header::CONTENT_TYPE,mime).header(header::CONTENT_LENGTH,(end-start+1).to_string()).header(header::ACCEPT_RANGES,"bytes");
    if partial{
        response=response.header(header::CONTENT_RANGE,format!("bytes {start}-{end}/{size}"));
    }
    response.body(Body::from_stream(stream)).map_err(storage_error)
}
enum Reader{
    Local(tokio::fs::File),S3(opendal::Operator,String)
}
impl Reader{
    async fn open(file:&file_storage::Model,start:u64)->KabiPayResult<Self>{
        match file.provider.as_str(){
            "LOCAL"=>{
                let mut file=tokio::fs::File::open(announcement_storage::absolute_storage_path(&file.storage_path)?).await.map_err(storage_error)?;
                file.seek(std::io::SeekFrom::Start(start)).await.map_err(storage_error)?;
                Ok(Self::Local(file))
            },"S3"=>{
                let cfg=S3CompatSettings::from_env()?;
                let op=s3_operator_for_bucket(&cfg,file.bucket.as_deref().ok_or_else(||bad("missing bucket"))?)?;
                Ok(Self::S3(op,file.storage_path.clone()))
            },_=>Err(bad("unsupported media provider"))
        }
    }
    async fn read(&mut self,start:u64,count:usize)->KabiPayResult<Vec<u8>>{
        match self{
            Self::Local(file)=>{
                let mut bytes=vec![0;
                count];
                let read=file.read(&mut bytes).await.map_err(storage_error)?;
                bytes.truncate(read);
                Ok(bytes)
            },Self::S3(op,key)=>op.read_with(key).range(start..start+count as u64).await.map(|b|b.to_vec()).map_err(storage_error)
        }
    }
}
#[cfg(test)]
mod tests{
    use super::*;
    #[tokio::test] async fn local_range_response_has_exact_bytes_and_headers(){
        let path=std::env::temp_dir().join(format!("kabipay-video-range-{}",uuid::Uuid::new_v4()));
        tokio::fs::write(&path,(0u8..100).collect::<Vec<_>>()).await.unwrap();
        let mut file=tokio::fs::File::open(&path).await.unwrap();
        file.seek(std::io::SeekFrom::Start(10)).await.unwrap();
        let response=secure(stream_response(Reader::Local(file),"video/mp4".into(),100,10,19,true).unwrap());
        assert_eq!(response.status(),StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()[header::CONTENT_RANGE],"bytes 10-19/100");
        assert_eq!(response.headers()[header::CONTENT_LENGTH],"10");
        assert_eq!(response.headers()[header::ACCEPT_RANGES],"bytes");
        assert_eq!(response.headers()[header::CACHE_CONTROL],"private, no-store");
        let bytes=axum::body::to_bytes(response.into_body(),100).await.unwrap();
        assert_eq!(bytes.as_ref(),&(10u8..20).collect::<Vec<_>>());
        tokio::fs::remove_file(path).await.unwrap();
    }
}
#[cfg(test)]
mod upload_tests {
    use super::*;
    #[tokio::test] async fn raw_upload_rejects_oversize_truncation_and_mime_mismatch(){
        for (bytes,expected,mime) in [(vec![0;
        13],12,"video/mp4"),(vec![0;
        11],12,"video/mp4"),(b"\0\0\0\x18ftypisom".to_vec(),12,"video/webm")] {
            let path=std::env::temp_dir().join(format!("kabipay-video-upload-{}",uuid::Uuid::new_v4()));
            let mut sink=Sink::Local(tokio::fs::File::create(&path).await.unwrap());
            assert!(write_body(&mut sink,Body::from(bytes),expected,mime).await.is_err());
            drop(sink);
            assert!(tokio::fs::metadata(&path).await.unwrap().len()<=expected);
            tokio::fs::remove_file(path).await.unwrap();
        }
    }
    #[tokio::test] async fn raw_upload_persists_valid_chunked_container(){
        let path=std::env::temp_dir().join(format!("kabipay-video-upload-{}",uuid::Uuid::new_v4()));
        let mut sink=Sink::Local(tokio::fs::File::create(&path).await.unwrap());
        let body=Body::from_stream(futures_util::stream::iter([Ok::<_,std::io::Error>(b"\0\0\0\x18".to_vec()),Ok(b"ftypisom".to_vec())]));
        write_body(&mut sink,body,12,"video/mp4").await.unwrap();
        drop(sink);
        assert_eq!(tokio::fs::read(&path).await.unwrap(),b"\0\0\0\x18ftypisom");
        tokio::fs::remove_file(path).await.unwrap();
    }
}
pub async fn response_headers(request:axum::http::Request<Body>,next:axum::middleware::Next)->Response {
    secure(next.run(request).await)
}
#[cfg(test)]
mod tenant_control_plane_tests {
    use super::*;
    #[tokio::test]
    async fn unavailable_control_plane_denies_upload_before_reading_body_and_denies_playback() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let pool = sea_orm::sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused").unwrap();
        pool.close().await;
        let state = MediaState {
            ops: pool.into(),
            cache: TenantDbCache::new(),
            fallback: TenantDbConfig {
                db_host: "unreachable.invalid".into(), db_port: 1,
                db_name: "unused".into(), db_user: "unused".into(),
                db_password: "unused".into(), schema_name: "unused".into(),
            },
        };
        let mut ticket = video::MediaTicket {
            tenant: uuid::Uuid::new_v4(), user: uuid::Uuid::new_v4(),
            resource: uuid::Uuid::new_v4(), purpose: "announcement-video-upload".into(),
            exp: chrono::Utc::now().timestamp() + 60,
        };
        let polled = Arc::new(AtomicBool::new(false));
        let observed = polled.clone();
        let body = Body::from_stream(futures_util::stream::once(async move {
            observed.store(true, Ordering::SeqCst);
            Ok::<_, std::io::Error>(vec![0u8; 12])
        }));
        assert!(upload_inner(&state, &video::sign_ticket(&ticket).unwrap(), HeaderMap::new(), body).await.is_err());
        assert!(!polled.load(Ordering::SeqCst));
        ticket.purpose = "announcement-video-play".into();
        assert!(play_inner(&state, &video::sign_ticket(&ticket).unwrap(), HeaderMap::new()).await.is_err());
    }
}
