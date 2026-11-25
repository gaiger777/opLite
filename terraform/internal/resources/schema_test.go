package resources

import (
	"context"
	"encoding/json"
	"os"
	"sort"
	"strings"
	"testing"
)

// The resource schemas must match the server's manifest schemas (OpenAPI `Manifest{Kind}` → spec):
// every spec field is an attribute (or listed in Excluded with a reason), with the same type class and
// required-ness. A field the server adds and the provider does not know fails here, with the list.
//
// ORBIVISOR_OPENAPI is the OpenAPI document: `cargo run -p orbivisor-api --bin orbivisor-api-openapi > openapi.json`.
func TestSchemasMatchTheServer(t *testing.T) {
	file := os.Getenv("ORBIVISOR_OPENAPI")
	if file == "" {
		t.Skip("ORBIVISOR_OPENAPI is not set (cargo run -p orbivisor-api --bin orbivisor-api-openapi > openapi.json)")
	}
	raw, err := os.ReadFile(file)
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		Components struct {
			Schemas map[string]map[string]any `json:"schemas"`
		} `json:"components"`
	}
	if err := json.Unmarshal(raw, &doc); err != nil {
		t.Fatal(err)
	}
	o := openapi{schemas: doc.Components.Schemas}
	for _, k := range Kinds {
		t.Run(k.TypeName, func(t *testing.T) {
			m, ok := o.schemas["Manifest"+k.Manifest]
			if !ok {
				t.Fatalf("no schema Manifest%s", k.Manifest)
			}
			spec := o.resolve(m["properties"].(map[string]any)["spec"].(map[string]any))
			props, _ := spec["properties"].(map[string]any)
			required := map[string]bool{}
			if r, ok := spec["required"].([]any); ok {
				for _, x := range r {
					required[x.(string)] = true
				}
			}
			fields := map[string]Field{}
			for _, f := range k.Fields {
				fields[f.Name] = f
			}
			var missing, unknown, wrong []string
			for _, name := range sortedKeys(props) {
				if _, ok := k.Excluded[name]; ok {
					continue
				}
				f, ok := fields[name]
				if !ok {
					missing = append(missing, name)
					continue
				}
				want := o.class(props[name].(map[string]any))
				if want != f.Type {
					wrong = append(wrong, name+": server "+want.String()+", provider "+f.Type.String())
				}
				if required[name] && !f.Required && !f.Defaulted {
					wrong = append(wrong, name+": required by the server")
				}
				if f.Required && !required[name] {
					wrong = append(wrong, name+": optional on the server")
				}
			}
			for name := range fields {
				if _, ok := props[name]; !ok {
					unknown = append(unknown, name)
				}
			}
			for name := range k.Excluded {
				if _, ok := props[name]; !ok {
					unknown = append(unknown, name+" (excluded)")
				}
			}
			sort.Strings(unknown)
			if len(missing)+len(unknown)+len(wrong) > 0 {
				t.Errorf("schema mismatch for %s\n  server fields the provider lacks: %v\n  provider fields the server lacks: %v\n  differences: %v",
					k.Manifest, missing, unknown, wrong)
			}
		})
	}
}

type openapi struct {
	schemas map[string]map[string]any
}

func (o openapi) resolve(s map[string]any) map[string]any {
	for i := 0; i < 16; i++ {
		ref, ok := s["$ref"].(string)
		if !ok {
			break
		}
		s = o.schemas[strings.TrimPrefix(ref, "#/components/schemas/")]
	}
	if all, ok := s["allOf"].([]any); ok && len(all) == 1 {
		return o.resolve(all[0].(map[string]any))
	}
	return s
}

// class is the Terraform type class of a JSON schema: primitives, string lists and string maps are
// typed; everything else (objects, lists of objects, unions) is dynamic.
func (o openapi) class(s map[string]any) FieldType {
	s = o.resolve(s)
	if one, ok := s["oneOf"].([]any); ok {
		seen := map[FieldType]bool{}
		for _, x := range one {
			x := o.resolve(x.(map[string]any))
			if x["type"] == "null" {
				continue
			}
			seen[o.class(x)] = true
		}
		if len(seen) == 1 {
			for c := range seen {
				return c
			}
		}
		return Dynamic
	}
	t := s["type"]
	if list, ok := t.([]any); ok {
		var nonNull []any
		for _, x := range list {
			if x != "null" {
				nonNull = append(nonNull, x)
			}
		}
		if len(nonNull) != 1 {
			return Dynamic
		}
		t = nonNull[0]
	}
	switch t {
	case "string":
		return String
	case "integer":
		return Int
	case "number":
		return Number
	case "boolean":
		return Bool
	case "array":
		if items, ok := s["items"].(map[string]any); ok && o.class(items) == String {
			return StringList
		}
		return Dynamic
	case "object":
		if ap, ok := s["additionalProperties"].(map[string]any); ok && s["properties"] == nil && o.class(ap) == String {
			return StringMap
		}
		return Dynamic
	}
	if _, ok := s["enum"]; ok {
		return String
	}
	return Dynamic
}

// The schemas are valid framework schemas and the attributes do not collide.
func TestSchemasAreValid(t *testing.T) {
	ctx := context.Background()
	if len(Kinds) != 15 {
		t.Fatalf("%d resources, want 15", len(Kinds))
	}
	seen := map[string]bool{}
	for _, k := range Kinds {
		if seen[k.TypeName] {
			t.Fatalf("duplicate %s", k.TypeName)
		}
		seen[k.TypeName] = true
		s := Schema(k)
		if d := s.ValidateImplementation(ctx); d.HasError() {
			t.Errorf("%s: %v", k.TypeName, d)
		}
		n := 3 // id, name, labels
		if k.Folder {
			n++
		}
		if k.State {
			n++
		}
		n += len(k.Fields) + 2*len(k.Secrets)
		if len(s.Attributes) != n {
			t.Errorf("%s: %d attributes, want %d (a field collides with a fixed attribute)", k.TypeName, len(s.Attributes), n)
		}
	}
}
