import json, socket, sys
def conn(p):
    s = socket.socket(socket.AF_UNIX); s.connect(p); return s
dec = json.JSONDecoder()
class C:
    def __init__(s, p): s.s = conn(p); s.buf = ""; s.id = 0
    def call(s, method, params, name):
        s.id += 1
        req = {"method": method, "params": params, "id": s.id}
        s.s.sendall(json.dumps(req).encode())
        while True:
            try:
                msg, end = dec.raw_decode(s.buf.lstrip()); s.buf = s.buf.lstrip()[end:]
                if msg.get("id") == s.id:
                    print(json.dumps({"name": name, "request": req, "response": msg})); sys.stdout.flush(); return msg
                print(json.dumps({"name": name + "/unsolicited", "response": msg}))
            except ValueError:
                d = s.s.recv(65536)
                if not d: raise SystemExit("closed")
                s.buf += d.decode()
leader, follower = C(sys.argv[1]), C(sys.argv[2])
srv = lambda: [{"op": "select", "table": "Database", "where": [["name", "==", "OVN_Northbound"]], "columns": ["name", "model", "connected", "leader", "cid", "sid", "index"]}]
leader.call("transact", ["_Server"] + srv(), "server-leader")
follower.call("transact", ["_Server"] + srv(), "server-follower")
leader.call("echo", ["x"], "echo")
leader.call("list_dbs", [], "list-dbs")
leader.call("transact", ["OVN_Northbound", {"op": "select", "table": "NB_Global", "where": [], "columns": ["_uuid", "external_ids", "nb_cfg", "hv_cfg"]}], "nb-global-empty")
leader.call("transact", ["OVN_Northbound",
  {"op": "wait", "table": "NB_Global", "where": [], "columns": ["external_ids"], "until": "==", "rows": [], "timeout": 0},
  {"op": "insert", "table": "NB_Global", "row": {"external_ids": ["map", [["orbivisor:t1:fence", "3:17"]]], "nb_cfg": 1}}], "nb-global-insert")
r = leader.call("transact", ["OVN_Northbound", {"op": "select", "table": "NB_Global", "where": [], "columns": ["_uuid", "external_ids", "nb_cfg", "hv_cfg"]}], "nb-global-select")
leader.call("transact", ["OVN_Northbound",
  {"op": "wait", "table": "NB_Global", "where": [], "columns": ["external_ids"], "until": "==", "rows": [{"external_ids": ["map", [["orbivisor:t1:fence", "2:1"]]]}], "timeout": 0},
  {"op": "mutate", "table": "NB_Global", "where": [], "mutations": [["nb_cfg", "+=", 1]]}], "wait-fails")
leader.call("transact", ["OVN_Northbound",
  {"op": "insert", "table": "Logical_Switch_Port", "uuid-name": "p0", "row": {"name": "opl-a-0", "addresses": ["set", ["02:00:00:00:00:01 10.0.0.5"]], "external_ids": ["map", [["orbivisor:cluster", "t1"]]]}},
  {"op": "insert", "table": "Logical_Switch", "uuid-name": "ls0", "row": {"name": "opl-net", "ports": ["set", [["named-uuid", "p0"]]], "external_ids": ["map", [["orbivisor:cluster", "t1"], ["orbivisor:id", "n1"]]]}},
  {"op": "insert", "table": "DHCP_Options", "row": {"cidr": "10.0.0.0/24", "options": ["map", [["lease_time", "86400"]]], "external_ids": ["map", [["orbivisor:cluster", "t1"]]]}}], "insert-switch")
leader.call("transact", ["OVN_Northbound", {"op": "select", "table": "Logical_Switch", "where": [["external_ids", "includes", ["map", [["orbivisor:cluster", "t1"]]]]], "columns": ["_uuid", "name", "ports", "external_ids", "other_config"]}], "select-switches")
leader.call("transact", ["OVN_Northbound", {"op": "select", "table": "Logical_Switch_Port", "where": [["external_ids", "includes", ["map", [["orbivisor:cluster", "t1"]]]]], "columns": ["_uuid", "name", "addresses", "port_security", "dhcpv4_options", "enabled", "options", "external_ids", "up"]}], "select-ports")
leader.call("transact", ["OVN_Northbound", {"op": "delete", "table": "Logical_Switch_Port", "where": [["name", "==", "opl-a-0"]]}], "delete-referenced")
leader.call("transact", ["OVN_Northbound", {"op": "insert", "table": "Logical_Switch_Port", "row": {"name": "orphan"}}], "insert-orphan")
leader.call("transact", ["OVN_Northbound", {"op": "select", "table": "Nope", "where": []}], "unknown-table")
follower.call("transact", ["OVN_Northbound", {"op": "insert", "table": "Logical_Switch", "row": {"name": "via-follower"}}], "follower-write")
follower.call("transact", ["OVN_Northbound", {"op": "select", "table": "Logical_Switch", "where": [], "columns": ["name"]}], "follower-read")
leader.call("transact", ["OVN_Northbound", {"op": "delete", "table": "Logical_Switch", "where": [["name", "==", "opl-net"]]}, {"op": "select", "table": "Logical_Switch_Port", "where": [], "columns": ["name"]}], "delete-switch-gc")
