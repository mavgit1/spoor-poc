import type { ApiGroup, Candidate, StatusSnapshot } from './types';

export function apiKey(origin: string, protocol: string): string {
  return `${origin}|${protocol}`;
}

export function buildApiGroups(candidates: Candidate[]): ApiGroup[] {
  const map = new Map<string, ApiGroup>();
  for (const c of candidates) {
    const key = apiKey(c.origin, c.protocol);
    let group = map.get(key);
    if (!group) {
      group = {
        key,
        origin: c.origin,
        protocol: c.protocol,
        ops: [],
        totalRequests: 0,
      };
      map.set(key, group);
    }
    group.ops.push(c);
    group.totalRequests += c.request_count ?? 0;
  }
  return [...map.values()].sort((a, b) => b.totalRequests - a.totalRequests);
}

export function protocolLabel(protocol: string): string {
  switch (protocol) {
    case 'graphql':
      return 'GraphQL';
    case 'jsonrpc':
      return 'JSON-RPC';
    case 'websocket':
      return 'WebSocket';
    case 'form':
      return 'Form';
    case 'grpcweb':
      return 'gRPC-Web';
    case 'protobuf':
      return 'Protobuf';
    default:
      return 'REST';
  }
}

export function opNoun(protocol: string): string {
  if (
    protocol === 'graphql' ||
    protocol === 'jsonrpc' ||
    protocol === 'websocket'
  ) {
    return 'operations';
  }
  return 'endpoints';
}

export function preferencePattern(
  group: ApiGroup,
  pattern: string,
): string {
  if (
    group.protocol === 'graphql' ||
    group.protocol === 'jsonrpc' ||
    group.protocol === 'websocket' ||
    group.protocol === 'grpcweb' ||
    group.protocol === 'protobuf'
  ) {
    try {
      return `host:${new URL(group.origin).host}`;
    } catch {
      return `host:${group.origin}`;
    }
  }
  const trimmed = pattern.replace(/^\//, '');
  return trimmed ? `**/${trimmed}**` : `**/*`;
}

export function formatStatusLine(
  data: StatusSnapshot,
  apiCount: number,
  candidatesLoaded: boolean,
): string {
  const parts: string[] = [`${data.flow_count} captured`];
  if (!data.recording && !data.analyzing && data.flows_classified > 0) {
    parts[0] += ` · ${data.flows_classified} classified`;
    if (data.flows_filtered > 0) {
      parts[0] += ` · ${data.flows_filtered} filtered`;
    }
  }
  if (!data.recording && !data.analyzing && data.candidate_count > 0) {
    const proto: string[] = [];
    if (data.graphql_ops) proto.push(`${data.graphql_ops} GraphQL`);
    if (data.jsonrpc_ops) proto.push(`${data.jsonrpc_ops} JSON-RPC`);
    if (data.rest_endpoints) proto.push(`${data.rest_endpoints} REST`);
    if (data.websocket_ops) proto.push(`${data.websocket_ops} WS`);
    if (data.form_ops) proto.push(`${data.form_ops} form`);
    if (data.grpc_ops) proto.push(`${data.grpc_ops} gRPC/protobuf`);
    parts[0] += candidatesLoaded
      ? ` · ${apiCount} API(s) · ${data.candidate_count} ops (${proto.join(' / ') || 'mixed'})`
      : ` · ${data.candidate_count} ops discovered`;
  } else if (data.candidate_count > 0) {
    parts[0] += ` · ${data.candidate_count} ops`;
  } else if (!data.recording && !data.analyzing && data.flows_classified > 0) {
    parts[0] +=
      ' · 0 ops discovered — use the site (search, filters, scroll) then Stop again';
  }
  if (data.flows_capped) {
    parts[0] += ' · flow limit reached (older traffic kept)';
  }
  if (
    !data.recording &&
    !data.analyzing &&
    (data.undecoded_binary || data.websocket_frames || data.grpc_or_protobuf)
  ) {
    const cov: string[] = [];
    if (data.undecoded_binary) cov.push(`${data.undecoded_binary} undecoded binary`);
    if (data.websocket_frames) cov.push(`${data.websocket_frames} ws frames`);
    if (data.grpc_or_protobuf) cov.push(`${data.grpc_or_protobuf} grpc/protobuf`);
    parts[0] += ` · coverage: ${cov.join(', ')}`;
  }
  return parts[0];
}
