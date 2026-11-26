import { invoke } from "@tauri-apps/api/core";
import type { DatasetDetail, DatasetGetRequest } from "./generated/wire-primitives";

import type { AnnotationGetRequest, AnnotationView, AssetListRequest, AssetPage, DatasetListRequest, DatasetPage, DisplayLocator, MediaGetLocatorRequest } from "./generated/wire-primitives";
import { context } from "./workspace";
import type { OpenWorkspace } from "./workspace";

export function getDataset(workspace: OpenWorkspace, datasetId: string): Promise<DatasetDetail> {
  const request: DatasetGetRequest = { context: context(workspace, crypto.randomUUID()), dataset_id: datasetId };
  return invoke("dataset_get", { request });
}

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

export function getAnnotation(workspace: OpenWorkspace, assetId: string): Promise<AnnotationView> {
  const request: AnnotationGetRequest = {
    context: context(workspace, crypto.randomUUID()), asset_id: assetId,
  };
  return invoke("annotation_get", { request });
}

export function getDisplayLocator(workspace: OpenWorkspace, assetId: string): Promise<DisplayLocator> {
  const request: MediaGetLocatorRequest = {
    context: context(workspace, crypto.randomUUID()), asset_id: assetId, purpose: "DISPLAY",
  };
  return invoke("media_get_locator", { request });
}
