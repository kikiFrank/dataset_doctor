import { invoke } from "@tauri-apps/api/core";

import type { AssetListRequest, AssetPage, DatasetListRequest, DatasetPage } from "./generated/wire-primitives";
import { context } from "./workspace";
import type { OpenWorkspace } from "./workspace";

export function listDatasets(workspace: OpenWorkspace, cursor?: string): Promise<DatasetPage> {
  const request: DatasetListRequest = {
    context: context(workspace, crypto.randomUUID()), page_cursor: cursor, page_size: 25,
  };
  return invoke("dataset_list", { request });
}

export function listAssets(workspace: OpenWorkspace, datasetId: string, cursor?: string): Promise<AssetPage> {
  const request: AssetListRequest = {
    context: context(workspace, crypto.randomUUID()), dataset_id: datasetId, cursor, page_size: 40,
  };
  return invoke("asset_list", { request });
}
