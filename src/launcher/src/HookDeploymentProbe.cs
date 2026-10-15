using System.Reflection;
using System.Text;
using System.Text.Json;
using ScblLan;

namespace SplinterCellCNLauncher;

// Explicit self-test only. Synthetic game/Hook PE data plus the frozen bundled
// original are copied into isolated fixtures; no DLL is loaded or executed.
// Process-guard injection is internal and used solely for these fresh fixture directories.
internal static class HookDeploymentProbe
{
    internal static string Run(string parent)
    {
        string root=Path.Combine(Path.GetFullPath(parent),"hook-fixture-"+Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(root);
        int passed=0,failed=0;var lines=new List<string>();
        void Check(bool value,string name){lines.Add((value?"PASS ":"FAIL ")+name);if(value)passed++;else failed++;}
        string unsigned=Path.Combine(root,"unsigned.dll");File.WriteAllBytes(unsigned,Pe("original"));
        Check(!OriginalLoaderVerifier.IsAuthentic(unsigned),"unsigned synthetic loader is not claimed to be Ubisoft authentic");
        var deploy=typeof(GameRuntime).GetMethod("DeployCore",BindingFlags.Static|BindingFlags.NonPublic,null,[typeof(string),typeof(string),typeof(Func<bool>)],null);
        var restore=typeof(GameRuntime).GetMethod("RestoreCore",BindingFlags.Static|BindingFlags.NonPublic);
        if(deploy==null || restore==null)
        {
            Check(false,"unsigned replacement adoption policy unavailable");
            Check(false,"independent pre-LAN backup and forwarder policy unavailable");
            Check(false,"isolated deployment/restore regression entry unavailable");
            return Finish();
        }
        bool Attempt(Action act){try{act();return true;}catch(Exception e) when(e is TargetInvocationException or IOException or UnauthorizedAccessException){return false;}}
        void Deploy(string game,string runtime,bool running=false)=>deploy.Invoke(null,[Path.Combine(game,"Blacklist_DX11_game.exe"),runtime,(Func<bool>)(()=>running)]);
        void Restore(string game)=>restore.Invoke(null,[Path.Combine(game,"Blacklist_DX11_game.exe"),(Func<bool>)(()=>false)]);
        string runtime=Path.Combine(root,"runtime");Directory.CreateDirectory(runtime);
        string source=Path.Combine(runtime,"uplay_r1_loader.dll");
        void Payload(string version){File.WriteAllBytes(source,Pe("SCBL_HOOKS_CONFIG=scbl.toml.v1 "+version));File.WriteAllText(source+".sha256",GameRuntime.Hash(source));}
        Payload("A");
        string Game(string name,string marker="other launcher")
        {
            string g=Path.Combine(root,name);Directory.CreateDirectory(g);
            File.WriteAllBytes(Path.Combine(g,"Blacklist_DX11_game.exe"),Pe("game"));
            File.WriteAllBytes(Path.Combine(g,"uplay_r1_loader.dll"),Pe(marker));return g;
        }
        string Target(string g)=>Path.Combine(g,"uplay_r1_loader.dll");
        string Forwarder(string g)=>Path.Combine(g,"uplay_r1_loader.orig.dll");
        string Marker(string g)=>Path.Combine(g,"scbl-lan.install.json");
        JsonElement Receipt(string g)=>JsonDocument.Parse(File.ReadAllText(Marker(g))).RootElement;
        string Backup(string g)=>Path.Combine(g,Receipt(g).GetProperty("BackupRelativePath").GetString()!);
        const string repairHash="D098E458B935721478F618B9E2F2A0990EA0D9E907AA9DCAC0B5E78D99BB0B89";
        string[] OrigBackups(string g){string d=Path.Combine(g,".scbl-backups","orig");return Directory.Exists(d)?Directory.GetFiles(d,"*.dll"):[];}
        bool HasPreimage(string g,string hash)=>OrigBackups(g).Any(p=>GameRuntime.Hash(p)==hash);
        var game=Game("existing-launcher","SCBL_HOOKS_CONFIG=scbl.toml.v1 old");
        string old=GameRuntime.Hash(Target(game));File.Copy(unsigned,Forwarder(game));string forwardHash=GameRuntime.Hash(Forwarder(game));
        Check(Attempt(()=>Deploy(game,runtime)) && GameRuntime.Hash(Target(game))==GameRuntime.Hash(source),"replace unsigned other-launcher Hook with verified LAN payload");
        if(!File.Exists(Marker(game)))return Finish();
        Check(GameRuntime.Hash(Backup(game))==old,"pre-LAN other-launcher bytes preserved separately");
        Check(GameRuntime.Hash(Forwarder(game))==forwardHash,"existing forwarder retained despite different old Hook hash");
        Check(Receipt(game).GetProperty("ForwarderHash").GetString()==forwardHash,"receipt distinguishes forwarder from pre-LAN backup");
        string firstBackup=Backup(game);string a=GameRuntime.Hash(Target(game));
        // Identical bytes should be left untouched; differing external replacements are adopted below.
        var oldWrite=new DateTime(2001,1,1,0,0,0,DateTimeKind.Utc);
        File.SetLastWriteTimeUtc(Target(game),oldWrite);
        Check(Attempt(()=>Deploy(game,runtime)) && Backup(game)==firstBackup,"identical redeployment reuses backup");
        Check(GameRuntime.Hash(Target(game))==GameRuntime.Hash(source) && File.GetLastWriteTimeUtc(Target(game))==oldWrite,"identical Hook avoids unnecessary writes");
        Payload("B");
        using(var held=File.Open(Marker(game),FileMode.Open,FileAccess.Read,FileShare.ReadWrite))
        {
            Check(!Attempt(()=>Deploy(game,runtime)) && GameRuntime.Hash(Target(game))==a,"locked receipt blocks mutation");
        }
        Check(Attempt(()=>Deploy(game,runtime)) && GameRuntime.Hash(Target(game))==GameRuntime.Hash(source),"owned Hook upgrade retains pre-LAN backup");
        Check(Attempt(()=>Restore(game)) && GameRuntime.Hash(Target(game))==old,"restore returns exact other-launcher bytes");
        Deploy(game,runtime);File.WriteAllBytes(Target(game),Pe("new external loader"));string external=GameRuntime.Hash(Target(game));
        Check(Attempt(()=>Deploy(game,runtime)) && GameRuntime.Hash(Target(game))==GameRuntime.Hash(source) && GameRuntime.Hash(Backup(game))==external,"external replacement after LAN install is adopted with new backup");
        Check(GameRuntime.Hash(firstBackup)==old,"previous backup never overwritten during re-adoption");
        Check(Attempt(()=>Restore(game)) && GameRuntime.Hash(Target(game))==external,"restore latest external preimage");
        var clean=Game("unsigned-no-backup");string cleanHash=GameRuntime.Hash(Target(clean));
        Check(Attempt(()=>Deploy(clean,runtime)) && GameRuntime.Hash(Forwarder(clean))==cleanHash,"compatible unsigned standalone loader may supply forwarder");
        var recursive=Game("recursive","SCBL_HOOKS_CONFIG=scbl.toml.v1");string recursiveHash=GameRuntime.Hash(Target(recursive));
        Check(Attempt(()=>Deploy(recursive,runtime)) && GameRuntime.Hash(Target(recursive))==GameRuntime.Hash(source) && GameRuntime.Hash(Backup(recursive))==recursiveHash && File.Exists(Forwarder(recursive)),"old Hook without forwarder receives bundled original and exact preimage backup");
        var chained=Game("chain","uplay_r1_loader.orig.dll");
        Check(Attempt(()=>Deploy(chained,runtime)) && GameRuntime.Hash(Forwarder(chained))!=GameRuntime.Hash(Backup(chained)),"recursive loader is not copied as forwarder; bundled original is used");
        var damaged=Game("damaged-current");File.WriteAllText(Target(damaged),"not a PE image");File.Copy(unsigned,Forwarder(damaged));string damageHash=GameRuntime.Hash(Target(damaged));
        Check(Attempt(()=>Deploy(damaged,runtime)) && GameRuntime.Hash(Backup(damaged))==damageHash,"damaged old target replaceable when independent forwarder is valid");
        var invalid=Game("invalid-forwarder");File.WriteAllText(Forwarder(invalid),"not a PE image");
        string invalidHash=GameRuntime.Hash(Forwarder(invalid));
        Check(Attempt(()=>Deploy(invalid,runtime)) && GameRuntime.Hash(Forwarder(invalid))==repairHash,"invalid existing forwarder is replaced with verified bundled original");
        Check(HasPreimage(invalid,invalidHash),"invalid forwarder preimage is backed up exactly before replacement");
        var x64=Game("x64-forwarder");var data=Pe("x64");BitConverter.GetBytes((ushort)0x8664).CopyTo(data,0x84);File.WriteAllBytes(Forwarder(x64),data);
        Check(Attempt(()=>Deploy(x64,runtime)) && GameRuntime.Hash(Forwarder(x64))==repairHash,"incompatible x64 forwarder is repaired with x86 original");
        var missing=Game("missing-export");data=Pe("bad exports");Encoding.ASCII.GetBytes("XXXXX_Startup").CopyTo(data,0x280);File.WriteAllBytes(Forwarder(missing),data);
        Check(Attempt(()=>Deploy(missing,runtime)) && GameRuntime.Hash(Forwarder(missing))==repairHash,"forwarder missing required exports is repaired");
        var busy=Game("running");string busyHash=GameRuntime.Hash(Target(busy));
        Check(!Attempt(()=>Deploy(busy,runtime,true)) && GameRuntime.Hash(Target(busy))==busyHash,"running-game guard still rejects deployment");
        var locked=Game("locked");string lockHash=GameRuntime.Hash(Target(locked));
        using(var held=File.Open(Target(locked),FileMode.Open,FileAccess.Read,FileShare.Read))
            Check(!Attempt(()=>Deploy(locked,runtime)) && GameRuntime.Hash(Target(locked))==lockHash,"locked DLL not forcibly replaced");
        Check(Attempt(()=>Deploy(locked,runtime)),"retry from pending receipt after unlock succeeds");
        var badPackage=Game("bad-package");File.WriteAllText(source+".sha256",new string('0',64));
        Check(!Attempt(()=>Deploy(badPackage,runtime)) && !File.Exists(Marker(badPackage)),"packaged payload hash failure remains blocking");Payload("B");
        var legacy=Game("legacy-receipt");string legacyHash=GameRuntime.Hash(Target(legacy));File.Copy(Target(legacy),Forwarder(legacy));File.Copy(Target(legacy),Path.Combine(legacy,"uplay_r1_loader.scbl-lan.original.dll"));
        File.Copy(source,Target(legacy),true);File.WriteAllText(Marker(legacy),JsonSerializer.Serialize(new InstallReceipt(Target(legacy),legacyHash,GameRuntime.Hash(source),true)));
        Check(Attempt(()=>Deploy(legacy,runtime)) && Attempt(()=>Restore(legacy)) && GameRuntime.Hash(Target(legacy))==legacyHash,"legacy receipts and fixed original backup remain restorable");
        // An untrusted receipt cannot redirect restore to an arbitrary file.
        var tamper=JsonSerializer.Deserialize<Dictionary<string,object?>>(File.ReadAllText(Marker(clean)))!;tamper["BackupRelativePath"]="../unsigned.dll";File.WriteAllText(Marker(clean),JsonSerializer.Serialize(tamper));
        Check(!Attempt(()=>Restore(clean)),"receipt traversal cannot select arbitrary restore source");
        var noMain=Game("missing-main");File.Delete(Target(noMain));File.Copy(unsigned,Forwarder(noMain));
        bool deployedMissing=Attempt(()=>Deploy(noMain,runtime));
        Check(deployedMissing && File.Exists(Target(noMain)) && GameRuntime.Hash(Target(noMain))==GameRuntime.Hash(source),"missing main loader is deployed when orig exists");
        Check(deployedMissing && GameRuntime.Hash(Forwarder(noMain))==GameRuntime.Hash(unsigned),"missing main repair preserves existing compatible orig");
        Check(deployedMissing && Receipt(noMain).TryGetProperty("OriginalAbsent",out var absent) && absent.GetBoolean(),"receipt records an absent preimage instead of inventing original bytes");
        Check(deployedMissing && Attempt(()=>Deploy(noMain,runtime)),"redeploy missing-preimage installation remains idempotent");
        Check(deployedMissing && Attempt(()=>Restore(noMain)) && !File.Exists(Target(noMain)),"rollback restores original absence of main loader");
        Check(deployedMissing && Attempt(()=>Restore(noMain)) && !File.Exists(Target(noMain)),"absence rollback is repeatable");
        Check(deployedMissing && Attempt(()=>Deploy(noMain,runtime)) && File.Exists(Target(noMain)),"redeployment works after absence rollback");
        var neither=Game("neither-loader");File.Delete(Target(neither));
        bool neitherOk=Attempt(()=>Deploy(neither,runtime));
        Check(neitherOk && File.Exists(Target(neither)) && File.Exists(Forwarder(neither)),"both missing: bundled original and bundled Hook are deployed");
        const string bundledHash="D098E458B935721478F618B9E2F2A0990EA0D9E907AA9DCAC0B5E78D99BB0B89";
        Check(neitherOk && GameRuntime.Hash(Forwarder(neither))==bundledHash,"bundled original matches frozen local Ubisoft DLL SHA256");
        if(neitherOk){File.SetLastWriteTimeUtc(Forwarder(neither),oldWrite);File.SetLastWriteTimeUtc(Target(neither),oldWrite);}
        Check(neitherOk && Attempt(()=>Deploy(neither,runtime)) && File.GetLastWriteTimeUtc(Forwarder(neither))==oldWrite && File.GetLastWriteTimeUtc(Target(neither))==oldWrite,"identical repaired pair is not rewritten");
        var disappeared=Game("main-disappeared-after-install");string disappearedHash=GameRuntime.Hash(Target(disappeared));Deploy(disappeared,runtime);File.Delete(Target(disappeared));
        Check(Attempt(()=>Deploy(disappeared,runtime)) && Attempt(()=>Restore(disappeared)) && GameRuntime.Hash(Target(disappeared))==disappearedHash,"missing installed main repairs without losing original restore backup");
        var recovered=Game("lost-forwarder-with-receipt");string recoveredHash=GameRuntime.Hash(Target(recovered));Deploy(recovered,runtime);File.Delete(Forwarder(recovered));
        Check(Attempt(()=>Deploy(recovered,runtime)) && GameRuntime.Hash(Forwarder(recovered))==recoveredHash,"missing orig recovers compatible receipt backup before fallback");
        var absentBusy=Game("absent-while-running");File.Delete(Target(absentBusy));
        Check(!Attempt(()=>Deploy(absentBusy,runtime,true)) && !File.Exists(Target(absentBusy)) && !File.Exists(Forwarder(absentBusy)),"running game blocks both missing-file writes");
        var origDirectory=Game("orig-is-directory");Directory.CreateDirectory(Forwarder(origDirectory));string directoryHash=GameRuntime.Hash(Target(origDirectory));
        Check(!Attempt(()=>Deploy(origDirectory,runtime)) && GameRuntime.Hash(Target(origDirectory))==directoryHash,"orig directory collision fails without touching main DLL");
        var interrupted=Game("deleted-restore");string interruptedHash=GameRuntime.Hash(Target(interrupted));Deploy(interrupted,runtime);File.Delete(Target(interrupted));
        Check(Attempt(()=>Restore(interrupted)) && GameRuntime.Hash(Target(interrupted))==interruptedHash,"restore can recover original when current main is absent");
        if(neitherOk)
        {
            File.WriteAllBytes(Target(neither),Pe("external replacement after missing preimage"));string externalAbsentHash=GameRuntime.Hash(Target(neither));
            Check(!Attempt(()=>Restore(neither)) && GameRuntime.Hash(Target(neither))==externalAbsentHash,"absence rollback never deletes external replacement");
        }
        else Check(false,"absence rollback never deletes external replacement");
        var zero=Game("zero-original");File.WriteAllBytes(Forwarder(zero),[]);string zeroHash=GameRuntime.Hash(Forwarder(zero));
        Check(Attempt(()=>Deploy(zero,runtime)) && GameRuntime.Hash(Forwarder(zero))==repairHash && HasPreimage(zero,zeroHash),"zero-byte original is backed up and repaired");
        var hooked=Game("hooked-original");File.WriteAllBytes(Forwarder(hooked),Pe("SCBL_HOOKS_CONFIG=scbl.toml.v1"));string hookedHash=GameRuntime.Hash(Forwarder(hooked));
        Check(Attempt(()=>Deploy(hooked,runtime)) && GameRuntime.Hash(Forwarder(hooked))==repairHash && HasPreimage(hooked,hookedHash),"Hook incorrectly named orig is backed up and replaced, never recursively forwarded");
        var registered=Game("damaged-original-after-install");string registeredPreimage=GameRuntime.Hash(Target(registered));Deploy(registered,runtime);
        string registeredBackup=Backup(registered);File.WriteAllText(Forwarder(registered),"damaged after install");string firstDamage=GameRuntime.Hash(Forwarder(registered));
        File.SetLastWriteTimeUtc(Target(registered),oldWrite);
        bool repaired=Attempt(()=>Deploy(registered,runtime));
        Check(repaired && GameRuntime.Hash(Forwarder(registered))==repairHash && HasPreimage(registered,firstDamage),"damaged orig after recorded installation repairs and preserves its damaged preimage");
        Check(repaired && Backup(registered)==registeredBackup && GameRuntime.Hash(registeredBackup)==registeredPreimage && File.GetLastWriteTimeUtc(Target(registered))==oldWrite,"orig repair preserves prior Hook backup and skips identical main loader");
        int backupCount=OrigBackups(registered).Length;if(repaired)File.SetLastWriteTimeUtc(Forwarder(registered),oldWrite);
        Check(repaired && Attempt(()=>Deploy(registered,runtime)) && OrigBackups(registered).Length==backupCount && File.GetLastWriteTimeUtc(Forwarder(registered))==oldWrite,"repaired identical orig is not rewritten or backed up again");
        File.WriteAllText(Forwarder(registered),"second distinct corruption");string secondDamage=GameRuntime.Hash(Forwarder(registered));
        Check(Attempt(()=>Deploy(registered,runtime)) && HasPreimage(registered,firstDamage) && HasPreimage(registered,secondDamage),"later orig corruption keeps both exact preimages");
        Check(Attempt(()=>Restore(registered)) && GameRuntime.Hash(Target(registered))==registeredPreimage && GameRuntime.Hash(Forwarder(registered))==repairHash,"Hook rollback retains repaired original and restores exact main preimage");
        var absentBad=Game("main-absent-orig-bad");File.Delete(Target(absentBad));File.WriteAllText(Forwarder(absentBad),"bad and main absent");
        Check(Attempt(()=>Deploy(absentBad,runtime)) && GameRuntime.Hash(Forwarder(absentBad))==repairHash && GameRuntime.Hash(Target(absentBad))==GameRuntime.Hash(source),"missing main plus corrupt orig repairs both components");
        var origLocked=Game("locked-bad-orig");File.WriteAllText(Forwarder(origLocked),"locked original");string origLockedHash=GameRuntime.Hash(Forwarder(origLocked)),mainLockedHash=GameRuntime.Hash(Target(origLocked));
        using(var held=File.Open(Forwarder(origLocked),FileMode.Open,FileAccess.Read,FileShare.Read))
            Check(!Attempt(()=>Deploy(origLocked,runtime)) && GameRuntime.Hash(Forwarder(origLocked))==origLockedHash && GameRuntime.Hash(Target(origLocked))==mainLockedHash,"locked corrupt orig fails without modifying either DLL");
        Check(Attempt(()=>Deploy(origLocked,runtime)) && HasPreimage(origLocked,origLockedHash),"unlocking corrupt orig allows repair retry");
        var readonlyOrig=Game("readonly-bad-orig");File.WriteAllText(Forwarder(readonlyOrig),"readonly original");string readonlyHash=GameRuntime.Hash(Forwarder(readonlyOrig));
        File.SetAttributes(Forwarder(readonlyOrig),FileAttributes.ReadOnly);
        try{Check(!Attempt(()=>Deploy(readonlyOrig,runtime)) && GameRuntime.Hash(Forwarder(readonlyOrig))==readonlyHash,"read-only corrupt orig is not forcibly modified");}
        finally{File.SetAttributes(Forwarder(readonlyOrig),FileAttributes.Normal);}
        var blockedBackup=Game("orig-backup-blocked");File.WriteAllText(Forwarder(blockedBackup),"backup blocked");string blockedHash=GameRuntime.Hash(Forwarder(blockedBackup)),blockedMain=GameRuntime.Hash(Target(blockedBackup));
        Directory.CreateDirectory(Path.Combine(blockedBackup,".scbl-backups"));File.WriteAllText(Path.Combine(blockedBackup,".scbl-backups","orig"),"directory blocked");
        Check(!Attempt(()=>Deploy(blockedBackup,runtime)) && GameRuntime.Hash(Forwarder(blockedBackup))==blockedHash && GameRuntime.Hash(Target(blockedBackup))==blockedMain,"failed backup creation leaves both DLLs untouched");
        var busyBad=Game("busy-corrupt-orig");File.WriteAllText(Forwarder(busyBad),"busy damaged");string busyBadHash=GameRuntime.Hash(Forwarder(busyBad));
        Check(!Attempt(()=>Deploy(busyBad,runtime,true)) && GameRuntime.Hash(Forwarder(busyBad))==busyBadHash,"running game blocks corrupt orig repair");
        var changedOrig=Game("orig-changed-during-preparation");File.WriteAllText(Forwarder(changedOrig),"initial damage");int origChecks=0;
        bool changedResult=Attempt(()=>deploy.Invoke(null,[Path.Combine(changedOrig,"Blacklist_DX11_game.exe"),runtime,(Func<bool>)(()=>{if(++origChecks==2)File.WriteAllBytes(Forwarder(changedOrig),Pe("concurrent compatible original"));return false;})]));
        Check(!changedResult && origChecks>=2 && LoaderCompatibility.CanForward(Forwarder(changedOrig)) && !OrigBackups(changedOrig).Any(),"concurrent orig replacement before commit is preserved");
        var startedGame=Game("game-starts-during-repair");File.WriteAllText(Forwarder(startedGame),"initial busy damage");string startedHash=GameRuntime.Hash(Forwarder(startedGame));int busyChecks=0;
        Check(!Attempt(()=>deploy.Invoke(null,[Path.Combine(startedGame,"Blacklist_DX11_game.exe"),runtime,(Func<bool>)(()=>++busyChecks>=2)])) && GameRuntime.Hash(Forwarder(startedGame))==startedHash,"game starting during preparation blocks orig commit");
        Check(!Directory.EnumerateFiles(root,"*.repair.tmp",SearchOption.AllDirectories).Any(),"repair staging files are cleaned after success and failure");
        return Finish();
        string Finish()=>string.Join("\n",lines)+$"\nRESULT {passed} passed; {failed} failed; ISOLATED_FILES_ONLY; no DLL executed\n";
    }
    private static byte[] Pe(string marker)
    {
        var b=new byte[1536];
        void W16(int o,ushort n)=>BitConverter.GetBytes(n).CopyTo(b,o);
        void W32(int o,uint n)=>BitConverter.GetBytes(n).CopyTo(b,o);
        W16(0,0x5a4d);W32(0x3c,0x80);W32(0x80,0x4550);W16(0x84,0x14c);W16(0x86,1);W16(0x94,224);W16(0x96,0x2102);
        W16(0x98,0x10b);W32(0xb4,0x10000000);W32(0xb8,0x1000);W32(0xbc,0x200);W32(0xd0,0x2000);W32(0xd4,0x200);W16(0xdc,3);W32(0xf4,16);W32(0xf8,0x1000);W32(0xfc,0x100);
        Encoding.ASCII.GetBytes(".edata").CopyTo(b,0x178);W32(0x180,0x400);W32(0x184,0x1000);W32(0x188,0x400);W32(0x18c,0x200);W32(0x19c,0x40000040);
        W32(0x210,1);W32(0x214,2);W32(0x218,2);W32(0x21c,0x1040);W32(0x220,0x1050);W32(0x224,0x1060);
        W32(0x240,0x1200);W32(0x244,0x1201);W32(0x250,0x1080);W32(0x254,0x10a0);W16(0x260,0);W16(0x262,1);
        Encoding.ASCII.GetBytes("UPLAY_Startup\0").CopyTo(b,0x280);Encoding.ASCII.GetBytes("UPLAY_GetLastError\0").CopyTo(b,0x2a0);Encoding.ASCII.GetBytes(marker).CopyTo(b,0x400);return b;
    }
}
