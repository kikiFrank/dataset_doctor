import type { PathGrant, ReviewWriteAccessView } from "./api/generated/wire-primitives";

export function annotationWriteReady(access: ReviewWriteAccessView, grant: PathGrant | null, now = Date.now()): boolean {
  if (!access.authorization_required && !access.read_only) return true;
  return grant !== null && grant.allowed_operations.includes("REVIEW_COMMIT")
    && Date.parse(grant.expires_at) > now;
}

export function annotationWriteGrantId(
  access: ReviewWriteAccessView, grant: PathGrant | null, now = Date.now(),
): string | undefined {
  if (!access.authorization_required && !access.read_only) return undefined;
  if (grant && annotationWriteReady(access, grant, now)) return grant.grant_id;
  throw new Error("标注写入尚未授权或授权已过期，请重新授权。");
}
