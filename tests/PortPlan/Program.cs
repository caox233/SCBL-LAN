using System.Net;
using System.Net.Sockets;
using SplinterCellCNLauncher.Services;
int count=0;
void Check(bool ok,string label){if(!ok)throw new Exception(label);Console.WriteLine("PASS "+label);count++;}
var visited=new List<int>();
var p=HostPortPlan.Create("192.0.2.10",false,(ip,port,proto)=>visited.Add(port));
Check(p.ApiPort==50051 && visited.SequenceEqual(new[]{80,8000,21126,21127,50051}),"all production ports unchanged");
foreach(int port in new[]{80,8000,21126,21127,50051})foreach(int code in new[]{10013,10048})
{
 bool rejected=false;var probes=new List<int>();
 try{HostPortPlan.Create("192.0.2.10",false,(ip,n,proto)=>{probes.Add(n);if(n==port)throw new SocketException(code);},(n,proto)=>" OWNER_FIXTURE pid=42");}
 catch(IOException e){rejected=e.Message.Contains($":{port}")&&e.Message.Contains(code.ToString())&&e.Message.Contains("端口保持不变")&&e.Message.Contains("OWNER_FIXTURE");}
 Check(rejected && !probes.Any(n=>n is 25051 or 35051 or 45051),$"fixed port {port}, error {code}: explain and reject, never change ports");
}
bool invalid=false;try{HostPortPlan.Create("192.0.2.10",false,(ip,port,proto)=>throw new SocketException(10049));}catch(IOException e){invalid=e.Message.Contains("IP 已不属于本机");}Check(invalid,"stale adapter is actionable");
visited.Clear();var smoke=HostPortPlan.Create("127.0.0.1",true,(ip,port,proto)=>visited.Add(port));
Check(smoke.ApiPort==15051 && visited.SequenceEqual(new[]{18080,18000,22126,22127,15051}),"existing isolated-test ports only; production ports unchanged");
Console.WriteLine($"RESULT {count} fixed-port-policy checks passed.");
