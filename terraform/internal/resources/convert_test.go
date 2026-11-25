package resources

import (
	"context"
	"encoding/json"
	"math/big"
	"reflect"
	"strings"
	"testing"

	"github.com/hashicorp/terraform-plugin-framework/attr"
	"github.com/hashicorp/terraform-plugin-framework/types"
)

func decode(t *testing.T, s string) any {
	t.Helper()
	dec := json.NewDecoder(strings.NewReader(s))
	dec.UseNumber()
	var v any
	if err := dec.Decode(&v); err != nil {
		t.Fatal(err)
	}
	return v
}

func encode(t *testing.T, v any) string {
	t.Helper()
	b, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	return string(b)
}

func TestJSONRoundTrip(t *testing.T) {
	ctx := context.Background()
	in := `{"cpu":{"cores":2,"model":"host"},"disks":[{"bus":"virtio-blk","volume":"v1"}],"ha":true,"ratio":0.5,"size":1099511627776}`
	v := fromJSON(decode(t, in))
	out, err := toJSON(ctx, types.DynamicValue(v))
	if err != nil {
		t.Fatal(err)
	}
	if got := encode(t, out); got != in {
		t.Fatalf("round trip\n got %s\nwant %s", got, in)
	}
}

func TestToJSONTypedAndUnknown(t *testing.T) {
	ctx := context.Background()
	list := types.ListValueMust(types.StringType, []attr.Value{types.StringValue("a"), types.StringValue("b")})
	got, err := toJSON(ctx, list)
	if err != nil || encode(t, got) != `["a","b"]` {
		t.Fatalf("list: %v %v", got, err)
	}
	if got, _ := toJSON(ctx, types.Int64Value(1<<40)); encode(t, got) != "1099511627776" {
		t.Fatalf("int: %v", got)
	}
	if got, _ := toJSON(ctx, types.StringNull()); got != nil {
		t.Fatalf("null: %v", got)
	}
	if _, err := toJSON(ctx, types.StringUnknown()); err != errUnknown {
		t.Fatalf("unknown: %v", err)
	}
	obj := types.ObjectValueMust(map[string]attr.Type{"a": types.StringType, "b": types.NumberType},
		map[string]attr.Value{"a": types.StringValue("x"), "b": types.NumberUnknown()})
	if _, err := toJSON(ctx, types.DynamicValue(obj)); err != errUnknown {
		t.Fatalf("nested unknown: %v", err)
	}
	// A null object key is no key.
	obj = types.ObjectValueMust(map[string]attr.Type{"a": types.StringType, "b": types.NumberType},
		map[string]attr.Value{"a": types.StringValue("x"), "b": types.NumberNull()})
	if got, _ := toJSON(ctx, obj); encode(t, got) != `{"a":"x"}` {
		t.Fatalf("null key: %v", encode(t, got))
	}
}

// Read follows what the configuration declared: server-filled keys (MAC, CPU model) are left out,
// changed declared values come through, missing declared keys become null.
func TestProjectFollowsTheDeclaredShape(t *testing.T) {
	ctx := context.Background()
	declared := fromJSON(decode(t, `{"cpu":{"cores":1},"nics":[{"network":"user","model":"virtio"}]}`))
	live := decode(t, `{"cpu":{"cores":2,"model":"host","sockets":1},"nics":[{"network":"user","model":"virtio","mac":"52:54:00:00:00:01"},{"network":"lan","model":"e1000"}]}`)
	got, err := toJSON(ctx, project(ctx, live, declared))
	if err != nil {
		t.Fatal(err)
	}
	want := `{"cpu":{"cores":2},"nics":[{"model":"virtio","network":"user"},{"model":"e1000","network":"lan"}]}`
	if encode(t, got) != want {
		t.Fatalf("\n got %s\nwant %s", encode(t, got), want)
	}
	// Unchanged live value projects to exactly the declared value (no diff).
	same := decode(t, `{"cpu":{"cores":1,"model":"host"},"nics":[{"network":"user","model":"virtio","mac":"x"}]}`)
	if p := project(ctx, same, declared); !p.Equal(declared) {
		t.Fatalf("unchanged projected to %v", p)
	}
	// A declared key the server lost is null (drift: the plan sets it again).
	gone := project(ctx, decode(t, `{"nics":[]}`), declared)
	obj := gone.(types.Object)
	if !obj.Attributes()["cpu"].IsNull() {
		t.Fatalf("cpu should be null: %v", obj)
	}
	// Null prior stays null whatever the server has.
	if p := project(ctx, live, types.DynamicNull()); !p.IsNull() {
		t.Fatalf("null prior: %v", p)
	}
}

func TestProjectKeepsPrimitivesAndTypeChanges(t *testing.T) {
	ctx := context.Background()
	if p := project(ctx, decode(t, `true`), fromJSON(false)); !p.Equal(types.BoolValue(true)) {
		t.Fatalf("bool: %v", p)
	}
	n := project(ctx, json.Number("3"), types.NumberValue(big.NewFloat(1)))
	if !n.Equal(types.NumberValue(big.NewFloat(3))) {
		t.Fatalf("number: %v", n)
	}
	// `ha = true` became an object on the server: take the live value.
	o := project(ctx, decode(t, `{"group":"g1"}`), types.BoolValue(true))
	if _, ok := o.(types.Object); !ok {
		t.Fatalf("type change: %T", o)
	}
}

func TestRemovedKeysResetToDefaults(t *testing.T) {
	prior := decode(t, `{"cores":2,"model":"host","flags":{"a":true,"b":true},"list":[1,2]}`)
	next := decode(t, `{"cores":2,"flags":{"a":true},"list":[1]}`)
	got := encode(t, removedKeys(prior, next))
	want := `{"cores":2,"flags":{"a":true,"b":null},"list":[1],"model":null}`
	if got != want {
		t.Fatalf("\n got %s\nwant %s", got, want)
	}
	if got := encode(t, removedKeys(nil, next)); got != encode(t, next) {
		t.Fatalf("no prior: %s", got)
	}
}

func TestKeys(t *testing.T) {
	folder := KindByType("folder")
	if k := folder.Key("web", "/prod", map[string]any{"kind": "vm"}); k != "vm:/prod/web" {
		t.Fatal(k)
	}
	if k := folder.Key("prod", "", map[string]any{"kind": "network"}); k != "network:/prod" {
		t.Fatal(k)
	}
	if k := KindByType("vm").Key("web01", "/prod", nil); k != "web01" {
		t.Fatal(k)
	}
	grant := KindByType("grant")
	cases := map[string]map[string]any{
		"user:alice operator vm:/prod":       {"user": "alice", "role": "operator", "target": "vm:/prod/"},
		"group:ops viewer / no-propagate":    {"group": "ops", "role": "viewer", "target": "/", "propagate": false},
		"user:bob vm-admin vm/web01":         {"user": "bob", "role": "vm-admin", "target": "vm/web01", "propagate": true},
		"user:carol admin network:/dmz/edge": {"user": "carol", "role": "admin", "target": " network:/dmz/edge "},
	}
	for want, spec := range cases {
		if got := grant.Key("", "", spec); got != want {
			t.Errorf("grant key %v = %q, want %q", spec, got, want)
		}
	}
	if n := grantName("user:alice operator vm:/prod"); !strings.HasPrefix(n, "grant-") || len(n) != 18 {
		t.Fatal(n)
	}
}

func TestMinimalImport(t *testing.T) {
	spec := decode(t, `{"cpu":{"cores":1},"ha":false,"disks":[],"nics":[{"network":"user","mac":"52:54:00:00:00:01","ips":["10.0.0.5"]}],"cloud_init":{"user":"rocky","password":{"secretRef":"Vm/x/cloud_init.password"}},"snapshot_schedule":null}`).(map[string]any)
	got := encode(t, minimal(KindByType("vm"), spec))
	want := `{"cloud_init":{"user":"rocky"},"cpu":{"cores":1},"nics":[{"network":"user"}]}`
	if got != want {
		t.Fatalf("\n got %s\nwant %s", got, want)
	}
	only := decode(t, `{"cloud_init":{"password":{"secretRef":"x"}}}`).(map[string]any)
	if got := minimal(KindByType("vm"), only); !reflect.DeepEqual(got, map[string]any{}) {
		t.Fatalf("empty object left: %v", got)
	}
}

func TestSetAndGetPath(t *testing.T) {
	m := map[string]any{"backend": map[string]any{"kind": "s3"}}
	setPath(m, "backend.s3.secret_access_key", "k")
	if v, ok := getPath(m, "backend.s3.secret_access_key"); !ok || v != "k" {
		t.Fatal(m)
	}
	if _, ok := getPath(m, "backend.nope.x"); ok {
		t.Fatal("found a missing path")
	}
}
