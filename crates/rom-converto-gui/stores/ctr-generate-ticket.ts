import { makeOpStore } from "./_makeOpStore";

export const useCtrGenerateTicketStore = makeOpStore("ctr-generate-ticket", () => ({
  cdnDir: "",
  output: "",
}));
