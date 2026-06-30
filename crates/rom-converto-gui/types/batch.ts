export interface BatchItem {
  id: string;
  input: string;
  output: string;
  status: "pending" | "running" | "done" | "error" | "cancelled";
  error?: string;
  result?: string;
}
