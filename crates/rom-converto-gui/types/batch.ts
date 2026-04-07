export interface BatchItem {
  id: string;
  input: string;
  output: string;
  status: "pending" | "running" | "done" | "error";
  error?: string;
  result?: string;
}
