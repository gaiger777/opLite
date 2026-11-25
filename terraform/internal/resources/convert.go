package resources

import (
	"context"
	"encoding/json"
	"fmt"
	"math/big"
	"sort"

	"github.com/hashicorp/terraform-plugin-framework/attr"
	"github.com/hashicorp/terraform-plugin-framework/types"
	"github.com/hashicorp/terraform-plugin-framework/types/basetypes"
	"github.com/hashicorp/terraform-plugin-go/tftypes"
)

// errUnknown means a value is not known yet (planning with another resource's computed attribute).
var errUnknown = fmt.Errorf("value not known yet")

// toJSON turns a Terraform value into its JSON form (nil for null). Unknown values give errUnknown.
func toJSON(ctx context.Context, v attr.Value) (any, error) {
	if v == nil || v.IsNull() {
		return nil, nil
	}
	if v.IsUnknown() {
		return nil, errUnknown
	}
	switch x := v.(type) {
	case basetypes.StringValue:
		return x.ValueString(), nil
	case basetypes.BoolValue:
		return x.ValueBool(), nil
	case basetypes.Int64Value:
		return json.Number(fmt.Sprint(x.ValueInt64())), nil
	case basetypes.Int32Value:
		return json.Number(fmt.Sprint(x.ValueInt32())), nil
	case basetypes.Float64Value:
		return json.Number(big.NewFloat(x.ValueFloat64()).Text('g', -1)), nil
	case basetypes.NumberValue:
		return numberJSON(x.ValueBigFloat()), nil
	case basetypes.DynamicValue:
		return toJSON(ctx, x.UnderlyingValue())
	case basetypes.ListValue:
		return listJSON(ctx, x.Elements())
	case basetypes.SetValue:
		return listJSON(ctx, x.Elements())
	case basetypes.TupleValue:
		return listJSON(ctx, x.Elements())
	case basetypes.MapValue:
		return mapJSON(ctx, x.Elements())
	case basetypes.ObjectValue:
		return mapJSON(ctx, x.Attributes())
	}
	return nil, fmt.Errorf("unsupported value type %T", v)
}

func numberJSON(f *big.Float) json.Number {
	if f.IsInt() {
		i, _ := f.Int(nil)
		return json.Number(i.String())
	}
	return json.Number(f.Text('g', -1))
}

func listJSON(ctx context.Context, els []attr.Value) (any, error) {
	out := make([]any, 0, len(els))
	for _, e := range els {
		j, err := toJSON(ctx, e)
		if err != nil {
			return nil, err
		}
		out = append(out, j)
	}
	return out, nil
}

func mapJSON(ctx context.Context, m map[string]attr.Value) (any, error) {
	out := make(map[string]any, len(m))
	for k, e := range m {
		j, err := toJSON(ctx, e)
		if err != nil {
			return nil, err
		}
		// A null key is the same as no key on the server; keep it out of the document.
		if j != nil {
			out[k] = j
		}
	}
	return out, nil
}

// FromJSON is fromJSON for other packages (data sources).
func FromJSON(v any) attr.Value { return fromJSON(v) }

// fromJSON is the Terraform value of JSON `v` with no shape to follow (import, computed values):
// objects, tuples and primitives. Null object keys are dropped.
func fromJSON(v any) attr.Value {
	switch x := v.(type) {
	case nil:
		return types.DynamicNull()
	case string:
		return types.StringValue(x)
	case bool:
		return types.BoolValue(x)
	case json.Number:
		f, _, err := big.ParseFloat(string(x), 10, 512, big.ToNearestEven)
		if err != nil {
			return types.StringValue(string(x))
		}
		return types.NumberValue(f)
	case float64:
		return types.NumberValue(big.NewFloat(x))
	case []any:
		vals := make([]attr.Value, 0, len(x))
		typs := make([]attr.Type, 0, len(x))
		for _, e := range x {
			ev := fromJSON(e)
			if ev.IsNull() {
				ev = types.StringNull()
			}
			vals = append(vals, ev)
			typs = append(typs, ev.Type(context.Background()))
		}
		return types.TupleValueMust(typs, vals)
	case map[string]any:
		vals := map[string]attr.Value{}
		typs := map[string]attr.Type{}
		for k, e := range x {
			if e == nil {
				continue
			}
			ev := fromJSON(e)
			vals[k] = ev
			typs[k] = ev.Type(context.Background())
		}
		return types.ObjectValueMust(typs, vals)
	}
	return types.StringValue(fmt.Sprint(v))
}

// nullOf is a null value of type t.
func nullOf(ctx context.Context, t attr.Type) attr.Value {
	v, err := t.ValueFromTerraform(ctx, tftypesNull(ctx, t))
	if err != nil {
		return types.DynamicNull()
	}
	return v
}

func tftypesNull(ctx context.Context, t attr.Type) tftypes.Value {
	return tftypes.NewValue(t.TerraformType(ctx), nil)
}

// project follows the shape of `prior` (what the configuration declared) over the live JSON value:
// object keys the configuration did not declare are left out (the server fills MAC addresses, defaults
// and so on), list elements follow element by element, primitives take the live value. A null prior
// stays null. This is how Read sees changes made outside Terraform without reporting server defaults.
func project(ctx context.Context, live any, prior attr.Value) attr.Value {
	if prior == nil || prior.IsNull() || prior.IsUnknown() {
		return prior
	}
	switch p := prior.(type) {
	case basetypes.DynamicValue:
		inner := project(ctx, live, p.UnderlyingValue())
		if inner == nil || inner.IsNull() {
			if live == nil {
				return types.DynamicNull()
			}
			return types.DynamicValue(fromJSON(live))
		}
		return types.DynamicValue(inner)
	case basetypes.ObjectValue:
		m, ok := live.(map[string]any)
		if !ok {
			return liveOr(ctx, live, prior)
		}
		vals := map[string]attr.Value{}
		typs := map[string]attr.Type{}
		for k, pv := range p.Attributes() {
			var nv attr.Value
			if lv, ok := m[k]; ok && lv != nil {
				nv = project(ctx, lv, pv)
				if nv.IsNull() {
					nv = fromJSON(lv)
				}
			} else {
				nv = nullOf(ctx, pv.Type(ctx))
			}
			vals[k] = nv
			typs[k] = nv.Type(ctx)
		}
		return types.ObjectValueMust(typs, vals)
	case basetypes.MapValue:
		m, ok := live.(map[string]any)
		if !ok {
			return liveOr(ctx, live, prior)
		}
		et := p.ElementType(ctx)
		vals := map[string]attr.Value{}
		for k, lv := range m {
			if lv == nil {
				continue
			}
			pv, ok := p.Elements()[k]
			if !ok {
				pv = nullOf(ctx, et)
			}
			nv := coerce(ctx, lv, pv, et)
			if nv == nil {
				return liveOr(ctx, live, prior)
			}
			vals[k] = nv
		}
		out, d := types.MapValue(et, vals)
		if d.HasError() {
			return liveOr(ctx, live, prior)
		}
		return out
	case basetypes.TupleValue, basetypes.ListValue, basetypes.SetValue:
		arr, ok := live.([]any)
		if !ok {
			return liveOr(ctx, live, prior)
		}
		var priorEls []attr.Value
		switch q := prior.(type) {
		case basetypes.TupleValue:
			priorEls = q.Elements()
		case basetypes.ListValue:
			priorEls = q.Elements()
		case basetypes.SetValue:
			priorEls = q.Elements()
		}
		vals := make([]attr.Value, 0, len(arr))
		typs := make([]attr.Type, 0, len(arr))
		for i, lv := range arr {
			var nv attr.Value
			if i < len(priorEls) && lv != nil {
				nv = project(ctx, lv, priorEls[i])
			}
			if nv == nil || nv.IsNull() {
				nv = fromJSON(lv)
				if nv.IsNull() {
					nv = types.StringNull()
				}
			}
			vals = append(vals, nv)
			typs = append(typs, nv.Type(ctx))
		}
		// Keep a list a list when the elements still share the element type (typed attributes).
		if l, ok := prior.(basetypes.ListValue); ok {
			if out, d := types.ListValue(l.ElementType(ctx), vals); !d.HasError() {
				return out
			}
		}
		return types.TupleValueMust(typs, vals)
	default:
		return liveOr(ctx, live, prior)
	}
}

// liveOr is the live value in the prior's primitive type when it fits, or the live value as it is.
func liveOr(ctx context.Context, live any, prior attr.Value) attr.Value {
	if v := coerce(ctx, live, prior, prior.Type(ctx)); v != nil {
		return v
	}
	return fromJSON(live)
}

// coerce converts live JSON to type t (nil when it does not fit).
func coerce(ctx context.Context, live any, prior attr.Value, t attr.Type) attr.Value {
	if live == nil {
		return nullOf(ctx, t)
	}
	switch t.(type) {
	case basetypes.StringType:
		if s, ok := live.(string); ok {
			return types.StringValue(s)
		}
	case basetypes.BoolType:
		if b, ok := live.(bool); ok {
			return types.BoolValue(b)
		}
	case basetypes.NumberType:
		if n, ok := live.(json.Number); ok {
			if f, _, err := big.ParseFloat(string(n), 10, 512, big.ToNearestEven); err == nil {
				return types.NumberValue(f)
			}
		}
	case basetypes.Int64Type:
		if n, ok := live.(json.Number); ok {
			if i, err := n.Int64(); err == nil {
				return types.Int64Value(i)
			}
		}
	case basetypes.Float64Type:
		if n, ok := live.(json.Number); ok {
			if f, err := n.Float64(); err == nil {
				return types.Float64Value(f)
			}
		}
	case basetypes.DynamicType:
		return types.DynamicValue(project(ctx, live, prior))
	default:
		if prior != nil && !prior.IsNull() {
			v := project(ctx, live, prior)
			if v != nil && v.Type(ctx).Equal(t) {
				return v
			}
		}
	}
	return nil
}

// removedKeys marks with JSON null the object keys `prior` had and `next` does not, all the way down
// through objects (lists are replaced whole on the server). The server reads null as "back to the
// default"; a key that is simply absent keeps the server's value.
func removedKeys(prior, next any) any {
	pm, ok1 := prior.(map[string]any)
	nm, ok2 := next.(map[string]any)
	if !ok1 || !ok2 {
		return next
	}
	out := make(map[string]any, len(nm))
	for k, v := range nm {
		out[k] = removedKeys(pm[k], v)
	}
	for k := range pm {
		if _, ok := nm[k]; !ok {
			out[k] = nil
		}
	}
	return out
}

// sortedKeys is a map's keys in order (stable diagnostics).
func sortedKeys[V any](m map[string]V) []string {
	out := make([]string, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	sort.Strings(out)
	return out
}
