import { invoke } from "@tauri-apps/api/core";
import type { StagingListRequest, StagingPage, StagingState } from "./generated/wire-primitives";
import { context, type OpenWorkspace } from "./workspace";

export function listStaging(workspace: OpenWorkspace, dataset: string, status?: StagingState, cursor?: string): Promise<StagingPage> {
  const request: StagingListRequest = {
    context: context(workspace, crypto.randomUUID()),
    target_dataset_id: dataset, status, cursor, page_size: 25,
  };
  return invoke("staging_list", { request });
}
