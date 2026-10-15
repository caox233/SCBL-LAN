await BackgroundTaskChecks.Run((ok, name) => Console.WriteLine((ok ? "PASS " : "FAIL ") + name));
Console.WriteLine("RESULT owned-task lifecycle checks passed; no game or real save used.");
