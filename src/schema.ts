import { z } from "zod";

export const JobStatusSchema = z.enum([
  "working",
  "needs_input",
  "idle",
  "completed",
  "failed",
  "stopped",
]);

export const ProcessStateSchema = z.enum(["alive", "exited", "sleeping", "unknown"]);

export const PrRefSchema = z.object({
  url: z.string(),
  owner: z.string(),
  repo: z.string(),
  number: z.number(),
  status: z.string().default("unknown"),
});

export const BlockingRequestSchema = z.object({
  type: z.string(),
  message: z.string(),
  event: z.unknown().optional(),
  createdAt: z.string(),
});

export const JobSchema = z.object({
  id: z.string(),
  provider: z.literal("codex"),
  codexThreadId: z.string().nullable(),
  title: z.string(),
  initialPrompt: z.string(),
  repoRoot: z.string(),
  cwd: z.string(),
  dispatchCwd: z.string(),
  worktreePath: z.string().nullable(),
  worktreeBranch: z.string().nullable(),
  model: z.string().nullable(),
  profile: z.string().nullable(),
  approvalPolicy: z.enum(["untrusted", "on-request", "never"]),
  sandbox: z.enum(["read-only", "workspace-write", "danger-full-access"]),
  status: JobStatusSchema,
  processState: ProcessStateSchema,
  pid: z.number().nullable(),
  activeWorkerPid: z.number().nullable(),
  pinned: z.boolean(),
  manualOrder: z.number().nullable(),
  archived: z.boolean(),
  deleted: z.boolean(),
  lastSummary: z.string().nullable(),
  lastOutput: z.string().nullable(),
  blockingRequest: BlockingRequestSchema.nullable(),
  prRefs: z.array(PrRefSchema),
  createdAt: z.string(),
  updatedAt: z.string(),
  completedAt: z.string().nullable(),
  exitCode: z.number().nullable(),
  error: z.string().nullable().optional(),
});

export const StoreSchema = z.object({
  version: z.number(),
  jobs: z.record(z.string(), JobSchema),
  preferences: z.record(z.string(), z.unknown()),
});

export const InboxMessageSchema = z.object({
  type: z.literal("reply"),
  prompt: z.string(),
  timestamp: z.string().optional(),
});

export type JobStatus = z.infer<typeof JobStatusSchema>;
export type ProcessState = z.infer<typeof ProcessStateSchema>;
export type PrRef = z.infer<typeof PrRefSchema>;
export type BlockingRequest = z.infer<typeof BlockingRequestSchema>;
export type Job = z.infer<typeof JobSchema>;
export type Store = z.infer<typeof StoreSchema>;
export type InboxMessage = z.infer<typeof InboxMessageSchema>;

export type JobPatch = Partial<Job>;
