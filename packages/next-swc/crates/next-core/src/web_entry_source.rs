use anyhow::{anyhow, Result};
use turbo_tasks::{TryJoinIterExt, Value};
use turbo_tasks_env::ProcessEnvVc;
use turbo_tasks_fs::FileSystemPathVc;
use turbopack::ecmascript::EcmascriptModuleAssetVc;
use turbopack_core::{
    chunk::{availability_info::AvailabilityInfo, ChunkGroupVc, ChunkableAsset, ChunkableAssetVc},
    reference_type::{EntryReferenceSubType, ReferenceType},
    resolve::{origin::PlainResolveOriginVc, parse::RequestVc},
};
use turbopack_dev_server::{
    html::DevHtmlAssetVc,
    source::{asset_graph::AssetGraphContentSourceVc, ContentSourceVc},
};
use turbopack_node::execution_context::ExecutionContextVc;

use crate::{
    mode::NextMode,
    next_client::context::{
        get_client_asset_context, get_client_compile_time_info, get_client_runtime_entries,
        get_dev_client_chunking_context, ClientContextType,
    },
    next_config::NextConfigVc,
};

#[turbo_tasks::function]
pub async fn create_web_entry_source(
    project_root: FileSystemPathVc,
    execution_context: ExecutionContextVc,
    entry_requests: Vec<RequestVc>,
    client_root: FileSystemPathVc,
    env: ProcessEnvVc,
    eager_compile: bool,
    browserslist_query: &str,
    next_config: NextConfigVc,
) -> Result<ContentSourceVc> {
    let ty = Value::new(ClientContextType::Other);
    let mode = Value::new(NextMode::Development);
    let compile_time_info = get_client_compile_time_info(mode, browserslist_query);
    let context = get_client_asset_context(
        project_root,
        execution_context,
        compile_time_info,
        ty,
        mode,
        next_config,
    );
    let chunking_context = get_dev_client_chunking_context(
        project_root,
        client_root,
        compile_time_info.environment(),
        ty,
    );
    let entries =
        get_client_runtime_entries(project_root, env, ty, mode, next_config, execution_context);

    let runtime_entries = entries.resolve_entries(context);

    let origin = PlainResolveOriginVc::new(context, project_root.join("_")).as_resolve_origin();
    let entries = entry_requests
        .into_iter()
        .map(|request| async move {
            let ty = Value::new(ReferenceType::Entry(EntryReferenceSubType::Web));
            Ok(origin
                .resolve_asset(request, origin.resolve_options(ty.clone()), ty)
                .primary_assets()
                .await?
                .first()
                .copied())
        })
        .try_join()
        .await?;

    let chunk_groups: Vec<_> = entries
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, module)| async move {
            if let Some(ecmascript) = EcmascriptModuleAssetVc::resolve_from(module).await? {
                let chunk = ecmascript
                    .as_evaluated_chunk(chunking_context, (i == 0).then_some(runtime_entries));
                let chunk_group = ChunkGroupVc::from_chunk(chunk);
                Ok(chunk_group)
            } else if let Some(chunkable) = ChunkableAssetVc::resolve_from(module).await? {
                // TODO this is missing runtime code, so it's probably broken and we should also
                // add an ecmascript chunk with the runtime code
                Ok(ChunkGroupVc::from_chunk(chunkable.as_chunk(
                    chunking_context,
                    Value::new(AvailabilityInfo::Root {
                        current_availability_root: module,
                    }),
                )))
            } else {
                // TODO convert into a serve-able asset
                Err(anyhow!(
                    "Entry module is not chunkable, so it can't be used to bootstrap the \
                     application"
                ))
            }
        })
        .try_join()
        .await?;

    let entry_asset = DevHtmlAssetVc::new(client_root.join("index.html"), chunk_groups).into();

    let graph = if eager_compile {
        AssetGraphContentSourceVc::new_eager(client_root, entry_asset)
    } else {
        AssetGraphContentSourceVc::new_lazy(client_root, entry_asset)
    }
    .into();
    Ok(graph)
}
