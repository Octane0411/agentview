import { runCodexTurn } from "./codex.js";
import { appendJobEvent, getJob, updateJob } from "./store.js";

export async function workerMain(argv: string[]): Promise<void> {
  const [, , , jobId, mode = "run", ...rest] = argv;
  if (!jobId) throw new Error("Worker requires a job id");
  const job = await getJob(jobId);
  if (!job) throw new Error(`Unknown job: ${jobId}`);

  const prompt =
    rest.join(" ").trim() ||
    (mode === "run" ? job.initialPrompt : "Continue the previous task.");

  try {
    await runCodexTurn(jobId, prompt, { resume: mode !== "run" });
  } catch (error) {
    const err = error instanceof Error ? error : new Error(String(error));
    await appendJobEvent(jobId, {
      type: "worker_error",
      error: err.message,
      stack: err.stack,
      timestamp: new Date().toISOString(),
    });
    await updateJob(jobId, () => ({
      status: "failed",
      processState: "exited",
      pid: null,
      activeWorkerPid: null,
      error: err.message,
      lastSummary: `failed: ${err.message}`,
      completedAt: new Date().toISOString(),
    }));
    process.exitCode = 1;
  }
}
