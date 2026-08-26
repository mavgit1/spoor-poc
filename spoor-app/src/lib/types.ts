export type Candidate = {
  id: string;
  label: string;
  protocol: string;
  guessed_pattern: string;
  example: string;
  host: string;
  methods: string[];
  confidence: string;
  origin: string;
  request_count: number;
  default_selected: boolean;
  preference_ignored: boolean;
};

export type StatusSnapshot = {
  recording: boolean;
  analyzing: boolean;
  flow_count: number;
  spec_ready: boolean;
  candidate_count: number;
  graphql_ops: number;
  jsonrpc_ops: number;
  rest_endpoints: number;
  websocket_ops: number;
  form_ops: number;
  grpc_ops: number;
  traffic_graphql: number;
  traffic_jsonrpc: number;
  traffic_rest: number;
  traffic_websocket: number;
  flows_classified: number;
  flows_filtered: number;
  flows_capped: boolean;
  undecoded_binary: number;
  websocket_frames: number;
  grpc_or_protobuf: number;
  filters_config: string;
};

export type CandidatesSnapshot = {
  origins: string[];
  candidates: Candidate[];
};

export type GenerateSelection = {
  id: string;
  pattern?: string | null;
};

export type GenerateOutcome = {
  message: string;
  warnings: string[];
};

export type FilterOutcome = {
  message: string;
  config_path: string;
  action: string;
};

export type StoredSessionItem = {
  id: string;
  started_at: string;
  ended_at: string | null;
  flow_count: number;
  size_bytes: number;
  size_label: string;
  gzipped: boolean;
  flows_capped: boolean;
};

export type SessionsSnapshot = {
  sessions: StoredSessionItem[];
  store_path: string;
  keep: number;
  max_bytes: number;
  max_label: string;
  total_bytes: number;
  total_label: string;
};

export type DiscoverFinished = {
  ok: boolean;
  error: string | null;
  origins: string[];
  candidates: Candidate[];
};

export type OpState = {
  checked: boolean;
  pattern: string;
};

export type ApiGroup = {
  key: string;
  origin: string;
  protocol: string;
  ops: Candidate[];
  totalRequests: number;
};
