export * from "./types";

// Each op module calls registerOp() at import time. Add one line per op as its
// def lands (T8 compress, T9 extract/decrypt/encrypt/convert, T10 verify,
// T11 dat, T12 tools):
import "./compress";
import "./extract";
import "./decrypt";
import "./encrypt";
import "./convert";
import "./verify";
import "./tools";
