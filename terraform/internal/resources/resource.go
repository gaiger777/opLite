// Package resources holds the Orbivisor resources. Every resource is one manifest document: Create and
// Update apply it (POST /apply with manager "terraform"), Read is GET /manifests/{kind}/{key}, Delete is
// DELETE /manifests/{kind}/{key}. The resources differ only in their schema (kinds.go). The server
// plans; at `terraform plan` time ModifyPlan asks it for a dry run and shows blocked steps (quota,
// capacity, permissions, references…) as errors.
package resources

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"

	"github.com/hashicorp/terraform-plugin-framework/attr"
	"github.com/hashicorp/terraform-plugin-framework/diag"
	"github.com/hashicorp/terraform-plugin-framework/path"
	"github.com/hashicorp/terraform-plugin-framework/resource"
	"github.com/hashicorp/terraform-plugin-framework/resource/identityschema"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/boolplanmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/dynamicplanmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/int64planmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/listplanmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/planmodifier"
	"github.com/hashicorp/terraform-plugin-framework/resource/schema/stringplanmodifier"
	"github.com/hashicorp/terraform-plugin-framework/schema/validator"
	"github.com/hashicorp/terraform-plugin-framework/tfsdk"
	"github.com/hashicorp/terraform-plugin-framework/types"

	"github.com/gaiger777/opLite/terraform/internal/client"
)

// ProviderData is what the provider hands to resources and data sources.
type ProviderData struct {
	Client *client.Client
	// ForceConflicts takes over objects another manager (an apply set) holds.
	ForceConflicts bool
}

// New returns the resource constructor of kind k.
func New(k *Kind) func() resource.Resource {
	return func() resource.Resource { return &manifestResource{kind: k} }
}

// All are the constructors of the fifteen resources.
func All() []func() resource.Resource {
	out := make([]func() resource.Resource, 0, len(Kinds))
	for _, k := range Kinds {
		out = append(out, New(k))
	}
	return out
}

type manifestResource struct {
	kind *Kind
	data *ProviderData
}

var (
	_ resource.ResourceWithConfigure      = &manifestResource{}
	_ resource.ResourceWithModifyPlan     = &manifestResource{}
	_ resource.ResourceWithImportState    = &manifestResource{}
	_ resource.ResourceWithIdentity       = &manifestResource{}
	_ resource.ResourceWithValidateConfig = &manifestResource{}
)

func (r *manifestResource) Metadata(_ context.Context, req resource.MetadataRequest, resp *resource.MetadataResponse) {
	resp.TypeName = req.ProviderTypeName + "_" + r.kind.TypeName
}

func (r *manifestResource) Configure(_ context.Context, req resource.ConfigureRequest, resp *resource.ConfigureResponse) {
	if req.ProviderData == nil {
		return
	}
	d, ok := req.ProviderData.(*ProviderData)
	if !ok {
		resp.Diagnostics.AddError("Unexpected provider data", fmt.Sprintf("got %T", req.ProviderData))
		return
	}
	r.data = d
}

func (r *manifestResource) Schema(_ context.Context, _ resource.SchemaRequest, resp *resource.SchemaResponse) {
	resp.Schema = Schema(r.kind)
}

// Schema is the resource schema of kind k.
func Schema(k *Kind) schema.Schema {
	attrs := map[string]schema.Attribute{
		"id": schema.StringAttribute{
			Computed:            true,
			MarkdownDescription: "The manifest key: the name (folders `tree:/path`, grants the grant's content).",
			PlanModifiers:       []planmodifier.String{stringplanmodifier.UseStateForUnknown()},
		},
		"labels": schema.MapAttribute{
			ElementType:         types.StringType,
			Optional:            true,
			MarkdownDescription: "Labels. Terraform owns the object's labels: labels set elsewhere are removed on the next apply (server labels `orbivisor.io/…` stay).",
		},
	}
	if k.NoName {
		attrs["name"] = schema.StringAttribute{
			Computed:            true,
			MarkdownDescription: "The name the server exports this object under (derived from its content).",
			PlanModifiers:       []planmodifier.String{stringplanmodifier.UseStateForUnknown()},
		}
	} else {
		attrs["name"] = schema.StringAttribute{
			Required:            true,
			MarkdownDescription: "Name (unique per kind; DNS label). Changing it replaces the object.",
			PlanModifiers:       []planmodifier.String{stringplanmodifier.RequiresReplace()},
		}
	}
	if k.Folder {
		desc := "Folder path (`/prod/web`); empty is the tree's root. Changing it replaces the object (move it with `orbivisorctl folder move` and change the path here instead)."
		if k.Manifest == "Folder" {
			desc = "Parent folder path (`/prod`); empty is the tree's root. Changing it replaces the folder."
		}
		attrs["folder"] = schema.StringAttribute{
			Optional:            true,
			MarkdownDescription: desc,
			PlanModifiers:       []planmodifier.String{stringplanmodifier.RequiresReplace()},
		}
	}
	if k.State {
		attrs["state"] = schema.StringAttribute{
			Optional:            true,
			MarkdownDescription: "Power state: `running` or `stopped` (unset: Terraform does not touch the power).",
			Validators:          []validator.String{oneOf{"running", "stopped"}},
		}
	}
	for _, f := range k.Fields {
		attrs[f.Name] = fieldAttribute(f)
	}
	for _, s := range k.Secrets {
		attrs[s.Attribute] = schema.StringAttribute{
			Optional:            true,
			Sensitive:           true,
			WriteOnly:           true,
			MarkdownDescription: s.Description + " Needs Terraform 1.11+ (tested with OpenTofu 1.13).",
		}
		attrs[s.Attribute+"_version"] = schema.Int64Attribute{
			Optional:            true,
			MarkdownDescription: fmt.Sprintf("Change it to send `%s` again (the value itself is not stored, so Terraform cannot see it change).", s.Attribute),
		}
	}
	return schema.Schema{
		MarkdownDescription: k.Description + fmt.Sprintf("\n\nManifest kind `%s`: the attributes are the manifest spec's fields in the server's JSON shape. Attributes left out keep the server's value; removing one sets the server default.", k.Manifest),
		Attributes:          attrs,
	}
}

func fieldAttribute(f Field) schema.Attribute {
	desc := f.Description
	switch f.Type {
	case String:
		a := schema.StringAttribute{Required: f.Required, Optional: !f.Required, MarkdownDescription: desc}
		if f.Replace {
			a.PlanModifiers = []planmodifier.String{stringplanmodifier.RequiresReplace()}
		}
		return a
	case Int:
		a := schema.Int64Attribute{Required: f.Required, Optional: !f.Required, MarkdownDescription: desc}
		if f.Replace {
			a.PlanModifiers = []planmodifier.Int64{int64planmodifier.RequiresReplace()}
		}
		return a
	case Number:
		return schema.NumberAttribute{Required: f.Required, Optional: !f.Required, MarkdownDescription: desc}
	case Bool:
		a := schema.BoolAttribute{Required: f.Required, Optional: !f.Required, MarkdownDescription: desc}
		if f.Replace {
			a.PlanModifiers = []planmodifier.Bool{boolplanmodifier.RequiresReplace()}
		}
		return a
	case StringList:
		a := schema.ListAttribute{ElementType: types.StringType, Required: f.Required, Optional: !f.Required, MarkdownDescription: desc}
		if f.Replace {
			a.PlanModifiers = []planmodifier.List{listplanmodifier.RequiresReplace()}
		}
		return a
	case StringMap:
		return schema.MapAttribute{ElementType: types.StringType, Required: f.Required, Optional: !f.Required, MarkdownDescription: desc}
	default:
		a := schema.DynamicAttribute{Required: f.Required, Optional: !f.Required, MarkdownDescription: desc}
		if f.Replace {
			a.PlanModifiers = []planmodifier.Dynamic{dynamicplanmodifier.RequiresReplace()}
		}
		return a
	}
}

func (r *manifestResource) IdentitySchema(_ context.Context, _ resource.IdentitySchemaRequest, resp *resource.IdentitySchemaResponse) {
	resp.IdentitySchema = identityschema.Schema{
		Attributes: map[string]identityschema.Attribute{
			"key": identityschema.StringAttribute{
				RequiredForImport: true,
				Description:       "The manifest key or the object id: the name (folders `tree:/path`, grants the grant's content).",
			},
		},
	}
}

// getter reads attributes from a plan, a state or a configuration.
type getter interface {
	GetAttribute(ctx context.Context, p path.Path, target interface{}) diag.Diagnostics
}

// value reads one attribute as its framework value type.
func value(ctx context.Context, g getter, name string, t FieldType) (attr.Value, diag.Diagnostics) {
	p := path.Root(name)
	switch t {
	case String:
		var v types.String
		d := g.GetAttribute(ctx, p, &v)
		return v, d
	case Int:
		var v types.Int64
		d := g.GetAttribute(ctx, p, &v)
		return v, d
	case Number:
		var v types.Number
		d := g.GetAttribute(ctx, p, &v)
		return v, d
	case Bool:
		var v types.Bool
		d := g.GetAttribute(ctx, p, &v)
		return v, d
	case StringList:
		var v types.List
		d := g.GetAttribute(ctx, p, &v)
		return v, d
	case StringMap:
		var v types.Map
		d := g.GetAttribute(ctx, p, &v)
		return v, d
	default:
		var v types.Dynamic
		d := g.GetAttribute(ctx, p, &v)
		return v, d
	}
}

func str(ctx context.Context, g getter, name string) (types.String, diag.Diagnostics) {
	var v types.String
	d := g.GetAttribute(ctx, path.Root(name), &v)
	return v, d
}

// docInput is where a document comes from.
type docInput struct {
	plan   getter
	config getter
	// prior is the state before (nil on create).
	prior getter
}

// buildDoc makes the manifest document of a planned resource. A value not known yet gives errUnknown.
func (r *manifestResource) buildDoc(ctx context.Context, in docInput) (client.Manifest, diag.Diagnostics) {
	var diags diag.Diagnostics
	k := r.kind
	doc := client.Manifest{APIVersion: client.APIVersion, Kind: k.Manifest, Spec: map[string]any{}}
	for _, f := range k.Fields {
		v, d := value(ctx, in.plan, f.Name, f.Type)
		diags.Append(d...)
		if v.IsUnknown() {
			return doc, append(diags, unknownDiag())
		}
		j, err := toJSON(ctx, v)
		if err != nil {
			if errors.Is(err, errUnknown) {
				return doc, append(diags, unknownDiag())
			}
			diags.AddAttributeError(path.Root(f.Name), "Invalid value", err.Error())
			continue
		}
		var before any
		if in.prior != nil {
			pv, d := value(ctx, in.prior, f.Name, f.Type)
			diags.Append(d...)
			if before, err = toJSON(ctx, pv); err != nil {
				before = nil
			}
		}
		switch {
		case j == nil && before != nil:
			// Removed from the configuration: back to the server's default.
			doc.Spec[f.Name] = nil
		case j != nil:
			doc.Spec[f.Name] = removedKeys(before, j)
		}
	}
	if !k.NoName {
		name, d := str(ctx, in.plan, "name")
		diags.Append(d...)
		if name.IsUnknown() {
			return doc, append(diags, unknownDiag())
		}
		doc.Metadata.Name = name.ValueString()
	}
	if k.Folder {
		folder, d := str(ctx, in.plan, "folder")
		diags.Append(d...)
		if folder.IsUnknown() {
			return doc, append(diags, unknownDiag())
		}
		if f := folder.ValueString(); f != "" && f != "/" {
			doc.Metadata.Folder = f
		}
	}
	if k.State {
		state, d := str(ctx, in.plan, "state")
		diags.Append(d...)
		if state.IsUnknown() {
			return doc, append(diags, unknownDiag())
		}
		doc.State = state.ValueString()
	}
	var labels types.Map
	diags.Append(in.plan.GetAttribute(ctx, path.Root("labels"), &labels)...)
	if labels.IsUnknown() {
		return doc, append(diags, unknownDiag())
	}
	if !labels.IsNull() {
		doc.Metadata.Labels = map[string]string{}
		for key, v := range labels.Elements() {
			s, ok := v.(types.String)
			if !ok || s.IsUnknown() {
				return doc, append(diags, unknownDiag())
			}
			doc.Metadata.Labels[key] = s.ValueString()
		}
	}
	for _, s := range k.Secrets {
		if in.config == nil {
			break
		}
		wo, d := str(ctx, in.config, s.Attribute)
		diags.Append(d...)
		if wo.IsUnknown() {
			return doc, append(diags, unknownDiag())
		}
		if wo.IsNull() {
			continue
		}
		send := in.prior == nil
		if !send {
			var now, before types.Int64
			diags.Append(in.plan.GetAttribute(ctx, path.Root(s.Attribute+"_version"), &now)...)
			diags.Append(in.prior.GetAttribute(ctx, path.Root(s.Attribute+"_version"), &before)...)
			send = !now.Equal(before)
		}
		if send {
			setPath(doc.Spec, s.Path, wo.ValueString())
		}
	}
	if k.NoName {
		doc.Metadata.Name = grantName(k.Key("", "", doc.Spec))
	}
	return doc, diags
}

// unknownDiag marks "not all values known" (never shown: callers check for it).
func unknownDiag() diag.Diagnostic {
	return diag.NewWarningDiagnostic(unknownSummary, "")
}

const unknownSummary = "\x00unknown"

func isUnknown(d diag.Diagnostics) bool {
	for _, x := range d {
		if x.Summary() == unknownSummary {
			return true
		}
	}
	return false
}

func withoutUnknown(d diag.Diagnostics) diag.Diagnostics {
	var out diag.Diagnostics
	for _, x := range d {
		if x.Summary() != unknownSummary {
			out = append(out, x)
		}
	}
	return out
}

// setPath sets a dotted path in a JSON object, making objects on the way.
func setPath(m map[string]any, p string, v any) {
	parts := strings.Split(p, ".")
	for _, part := range parts[:len(parts)-1] {
		next, ok := m[part].(map[string]any)
		if !ok {
			next = map[string]any{}
			m[part] = next
		}
		m = next
	}
	m[parts[len(parts)-1]] = v
}

// getPath reads a dotted path.
func getPath(m map[string]any, p string) (any, bool) {
	var cur any = m
	for _, part := range strings.Split(p, ".") {
		obj, ok := cur.(map[string]any)
		if !ok {
			return nil, false
		}
		if cur, ok = obj[part]; !ok {
			return nil, false
		}
	}
	return cur, true
}

func (r *manifestResource) ValidateConfig(ctx context.Context, req resource.ValidateConfigRequest, resp *resource.ValidateConfigResponse) {
	// Secrets inside a dynamic attribute would land in the state: point at the write-only attribute.
	for _, s := range r.kind.Secrets {
		top, rest, _ := strings.Cut(s.Path, ".")
		var v types.Dynamic
		resp.Diagnostics.Append(req.Config.GetAttribute(ctx, path.Root(top), &v)...)
		j, err := toJSON(ctx, v)
		if err != nil || j == nil {
			continue
		}
		obj, ok := j.(map[string]any)
		if !ok {
			continue
		}
		if x, found := getPath(obj, rest); found && x != nil {
			if _, isRef := x.(map[string]any); isRef {
				continue
			}
			resp.Diagnostics.AddAttributeError(path.Root(top), "Secret in a plain attribute",
				fmt.Sprintf("%s would be stored in the Terraform state; give it with the write-only attribute %s instead.", s.Path, s.Attribute))
		}
	}
}

func (r *manifestResource) ModifyPlan(ctx context.Context, req resource.ModifyPlanRequest, resp *resource.ModifyPlanResponse) {
	if req.Plan.Raw.IsNull() || r.data == nil {
		return
	}
	creating := req.State.Raw.IsNull()
	in := docInput{plan: req.Plan, config: req.Config}
	if !creating {
		in.prior = req.State
		if req.Plan.Raw.Equal(req.State.Raw) {
			return
		}
		// Replacing: the old object still exists until Terraform deletes it, so a dry run now would
		// compare against it. Apply checks the new one.
		if len(resp.RequiresReplace) > 0 {
			return
		}
	}
	doc, diags := r.buildDoc(ctx, in)
	if isUnknown(diags) {
		// Another resource's value is not known yet: the server checks at apply time.
		return
	}
	resp.Diagnostics.Append(diags...)
	if diags.HasError() {
		return
	}
	key := r.kind.Key(doc.Metadata.Name, doc.Metadata.Folder, doc.Spec)
	if creating {
		resp.Diagnostics.Append(resp.Plan.SetAttribute(ctx, path.Root("id"), key)...)
		if r.kind.NoName {
			resp.Diagnostics.Append(resp.Plan.SetAttribute(ctx, path.Root("name"), doc.Metadata.Name)...)
		}
	}
	plan, err := r.data.Client.PlanOne(ctx, doc, r.data.ForceConflicts)
	if err != nil {
		resp.Diagnostics.AddError("Orbivisor could not plan "+r.kind.Manifest+" "+key, err.Error())
		return
	}
	resp.Diagnostics.Append(planDiagnostics(r.kind, plan, true)...)
}

// planDiagnostics turns the server plan's problems into diagnostics. At plan time a missing reference is
// a warning: the object may be one this Terraform run creates first (its name is known, so Terraform
// cannot tell us); apply checks again.
func planDiagnostics(k *Kind, plan *client.Plan, planning bool) diag.Diagnostics {
	var out diag.Diagnostics
	for _, st := range plan.Steps {
		what := fmt.Sprintf("%s %q", st.Kind, st.Name)
		for _, p := range st.Problems {
			summary := fmt.Sprintf("%s: %s", what, p.Code)
			detail := p.Message
			if p.Path != "" {
				detail += fmt.Sprintf(" (at %s)", p.Path)
			}
			at := attrPath(k, p.Path)
			switch {
			case p.Level == "warning":
				addWarning(&out, at, summary, detail)
			case planning && p.Code == "missing_reference":
				addWarning(&out, at, summary, detail+". If this Terraform run creates it, ignore this; apply checks again.")
			default:
				addError(&out, at, summary, detail)
			}
		}
		if st.Restart {
			out.AddWarning(what+": restart needed",
				"The VM is running: the hardware change is hot-plugged where possible, otherwise it applies at the next restart.")
		}
	}
	return out
}

func attrPath(k *Kind, p string) *path.Path {
	top, _, _ := strings.Cut(p, ".")
	switch top {
	case "metadata":
		if strings.HasSuffix(p, ".folder") && k.Folder {
			x := path.Root("folder")
			return &x
		}
		return nil
	case "":
		return nil
	}
	for _, f := range k.Fields {
		if f.Name == top {
			x := path.Root(top)
			return &x
		}
	}
	return nil
}

func addError(d *diag.Diagnostics, at *path.Path, summary, detail string) {
	if at != nil {
		d.AddAttributeError(*at, summary, detail)
	} else {
		d.AddError(summary, detail)
	}
}

func addWarning(d *diag.Diagnostics, at *path.Path, summary, detail string) {
	if at != nil {
		d.AddAttributeWarning(*at, summary, detail)
	} else {
		d.AddWarning(summary, detail)
	}
}

// apply sends the document and, on success, copies the plan into the state.
func (r *manifestResource) apply(ctx context.Context, in docInput, plan tfsdk.Plan, state *tfsdk.State) diag.Diagnostics {
	doc, diags := r.buildDoc(ctx, in)
	if isUnknown(diags) {
		return append(withoutUnknown(diags), diag.NewErrorDiagnostic("Unknown value at apply", "a value is still unknown"))
	}
	if diags.HasError() {
		return diags
	}
	key := r.kind.Key(doc.Metadata.Name, doc.Metadata.Folder, doc.Spec)
	p, err := r.data.Client.ApplyOne(ctx, doc, r.data.ForceConflicts)
	if err != nil {
		if blocked := client.BlockedPlan(err); blocked != nil {
			diags.Append(planDiagnostics(r.kind, blocked, false)...)
			if !diags.HasError() {
				diags.AddError("Orbivisor refused the change", err.Error())
			}
			return diags
		}
		diags.AddError("Orbivisor could not apply "+r.kind.Manifest+" "+key, err.Error())
		return diags
	}
	for _, st := range p.Steps {
		for _, pr := range st.Problems {
			if pr.Level == "warning" {
				diags.AddWarning(fmt.Sprintf("%s %q: %s", st.Kind, st.Name, pr.Code), pr.Message)
			}
		}
	}
	state.Raw = plan.Raw.Copy()
	diags.Append(state.SetAttribute(ctx, path.Root("id"), key)...)
	if r.kind.NoName {
		diags.Append(state.SetAttribute(ctx, path.Root("name"), doc.Metadata.Name)...)
	}
	return diags
}

func (r *manifestResource) Create(ctx context.Context, req resource.CreateRequest, resp *resource.CreateResponse) {
	resp.Diagnostics.Append(r.apply(ctx, docInput{plan: req.Plan, config: req.Config}, req.Plan, &resp.State)...)
	r.setIdentity(ctx, &resp.State, resp.Identity, &resp.Diagnostics)
}

func (r *manifestResource) Update(ctx context.Context, req resource.UpdateRequest, resp *resource.UpdateResponse) {
	resp.Diagnostics.Append(r.apply(ctx, docInput{plan: req.Plan, config: req.Config, prior: req.State}, req.Plan, &resp.State)...)
	r.setIdentity(ctx, &resp.State, resp.Identity, &resp.Diagnostics)
}

func (r *manifestResource) setIdentity(ctx context.Context, state *tfsdk.State, identity *tfsdk.ResourceIdentity, diags *diag.Diagnostics) {
	if identity == nil || diags.HasError() {
		return
	}
	id, d := str(ctx, state, "id")
	diags.Append(d...)
	diags.Append(identity.SetAttribute(ctx, path.Root("key"), id)...)
}

func (r *manifestResource) Delete(ctx context.Context, req resource.DeleteRequest, resp *resource.DeleteResponse) {
	id, d := str(ctx, req.State, "id")
	resp.Diagnostics.Append(d...)
	if d.HasError() {
		return
	}
	err := r.data.Client.DeleteManifest(ctx, r.kind.Manifest, id.ValueString())
	var e *client.APIError
	if r.kind.Manifest == "Vm" && errors.As(err, &e) && e.Code == "vm_is_template" {
		// A template is turned back into a VM (its Task) before it can be deleted.
		err = r.data.Client.Untemplate(ctx, id.ValueString())
		if err == nil {
			err = r.data.Client.DeleteManifest(ctx, r.kind.Manifest, id.ValueString())
		}
	}
	if err != nil {
		resp.Diagnostics.AddError("Orbivisor could not delete "+r.kind.Manifest+" "+id.ValueString(), err.Error())
	}
}

func (r *manifestResource) ImportState(ctx context.Context, req resource.ImportStateRequest, resp *resource.ImportStateResponse) {
	key := req.ID
	if key == "" && req.Identity != nil {
		var k types.String
		resp.Diagnostics.Append(req.Identity.GetAttribute(ctx, path.Root("key"), &k)...)
		key = k.ValueString()
	}
	if key == "" {
		resp.Diagnostics.AddError("Missing import key", "give the name (folders `tree:/path`) or the id")
		return
	}
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("id"), key)...)
}

func (r *manifestResource) Read(ctx context.Context, req resource.ReadRequest, resp *resource.ReadResponse) {
	id, d := str(ctx, req.State, "id")
	resp.Diagnostics.Append(d...)
	if d.HasError() {
		return
	}
	m, err := r.data.Client.GetManifest(ctx, r.kind.Manifest, id.ValueString())
	if errors.Is(err, client.ErrNotFound) {
		// Deleted outside Terraform: the next plan creates it again.
		resp.State.RemoveResource(ctx)
		return
	}
	if err != nil {
		resp.Diagnostics.AddError("Orbivisor could not read "+r.kind.Manifest+" "+id.ValueString(), err.Error())
		return
	}
	resp.Diagnostics.Append(r.fromManifest(ctx, req.State, &resp.State, m)...)
	r.setIdentity(ctx, &resp.State, resp.Identity, &resp.Diagnostics)
}

// fromManifest writes the live object into the state. After an import (no name in the prior state)
// every field the server has is taken; otherwise each attribute follows what the configuration
// declared (project) so server defaults and filled-in values do not show as changes.
func (r *manifestResource) fromManifest(ctx context.Context, prior tfsdk.State, state *tfsdk.State, m *client.Manifest) diag.Diagnostics {
	var diags diag.Diagnostics
	k := r.kind
	priorName, d := str(ctx, prior, "name")
	diags.Append(d...)
	importing := priorName.IsNull()
	spec := m.Spec
	if spec == nil {
		spec = map[string]any{}
	}
	if importing {
		spec = minimal(k, spec)
	}
	set := func(name string, v any) {
		diags.Append(state.SetAttribute(ctx, path.Root(name), v)...)
	}
	set("id", k.Key(m.Metadata.Name, m.Metadata.Folder, spec))
	set("name", m.Metadata.Name)
	if k.Folder {
		pf, d := str(ctx, prior, "folder")
		diags.Append(d...)
		switch {
		case m.Metadata.Folder != "":
			set("folder", m.Metadata.Folder)
		case pf.ValueString() == "/":
			set("folder", "/")
		default:
			set("folder", types.StringNull())
		}
	}
	if k.State {
		ps, d := str(ctx, prior, "state")
		diags.Append(d...)
		if (importing || !ps.IsNull()) && m.State != "" {
			set("state", m.State)
		} else {
			set("state", types.StringNull())
		}
	}
	var pl types.Map
	diags.Append(prior.GetAttribute(ctx, path.Root("labels"), &pl)...)
	if len(m.Metadata.Labels) == 0 && (pl.IsNull() || importing) {
		set("labels", types.MapNull(types.StringType))
	} else {
		lv, d := types.MapValueFrom(ctx, types.StringType, m.Metadata.Labels)
		diags.Append(d...)
		set("labels", lv)
	}
	for _, f := range k.Fields {
		pv, d := value(ctx, prior, f.Name, f.Type)
		diags.Append(d...)
		live, has := spec[f.Name]
		if !has {
			live = nil
		}
		var nv attr.Value
		switch {
		case importing:
			nv = typed(ctx, f.Type, live, nil)
		case pv.IsNull():
			nv = pv
		default:
			nv = typed(ctx, f.Type, live, pv)
		}
		diags.Append(state.SetAttribute(ctx, path.Root(f.Name), nv)...)
	}
	for _, s := range k.Secrets {
		var ver types.Int64
		diags.Append(prior.GetAttribute(ctx, path.Root(s.Attribute+"_version"), &ver)...)
		set(s.Attribute+"_version", ver)
		set(s.Attribute, types.StringNull())
	}
	return diags
}

// typed is the attribute value of live JSON in the field's type, following prior's shape.
func typed(ctx context.Context, t FieldType, live any, prior attr.Value) attr.Value {
	switch t {
	case String:
		if s, ok := live.(string); ok {
			return types.StringValue(s)
		}
		if live != nil {
			return types.StringValue(fmt.Sprint(live))
		}
		return types.StringNull()
	case Int:
		if n, ok := live.(json.Number); ok {
			if i, err := n.Int64(); err == nil {
				return types.Int64Value(i)
			}
		}
		return types.Int64Null()
	case Number:
		if v := coerce(ctx, live, nil, types.NumberType); v != nil {
			return v
		}
		return types.NumberNull()
	case Bool:
		if b, ok := live.(bool); ok {
			return types.BoolValue(b)
		}
		return types.BoolNull()
	case StringList:
		arr, ok := live.([]any)
		if !ok {
			return types.ListNull(types.StringType)
		}
		vals := make([]attr.Value, 0, len(arr))
		for _, e := range arr {
			s, _ := e.(string)
			vals = append(vals, types.StringValue(s))
		}
		return types.ListValueMust(types.StringType, vals)
	case StringMap:
		obj, ok := live.(map[string]any)
		if !ok {
			return types.MapNull(types.StringType)
		}
		vals := map[string]attr.Value{}
		for k, e := range obj {
			s, _ := e.(string)
			vals[k] = types.StringValue(s)
		}
		return types.MapValueMust(types.StringType, vals)
	default:
		if live == nil {
			return types.DynamicNull()
		}
		if prior == nil {
			return types.DynamicValue(fromJSON(live))
		}
		return project(ctx, live, prior)
	}
}

// minimal drops what an imported object should not pin in the configuration: empty values and the
// server-made VM MAC/IP addresses and floating IP address (the server's `minimal` export).
func minimal(k *Kind, spec map[string]any) map[string]any {
	out := map[string]any{}
	for key, v := range spec {
		if v = strip(v); v != nil {
			out[key] = v
		}
	}
	if k.Manifest == "Vm" {
		if nics, ok := out["nics"].([]any); ok {
			for _, n := range nics {
				if obj, ok := n.(map[string]any); ok {
					delete(obj, "mac")
					delete(obj, "ips")
				}
			}
		}
	}
	if k.Manifest == "FloatingIp" {
		delete(out, "address")
	}
	// Secrets come back as references ("keep the stored value"); they are write-only attributes here.
	for _, s := range k.Secrets {
		deletePath(out, s.Path)
	}
	return out
}

// deletePath removes a dotted path (and objects it leaves empty).
func deletePath(m map[string]any, p string) {
	head, rest, nested := strings.Cut(p, ".")
	if !nested {
		delete(m, head)
		return
	}
	if child, ok := m[head].(map[string]any); ok {
		deletePath(child, rest)
		if len(child) == 0 {
			delete(m, head)
		}
	}
}

func strip(v any) any {
	switch x := v.(type) {
	case nil:
		return nil
	case bool:
		if !x {
			return nil
		}
	case string:
		if x == "" {
			return nil
		}
	case []any:
		if len(x) == 0 {
			return nil
		}
		for i, e := range x {
			if s := strip(e); s != nil {
				x[i] = s
			}
		}
	case map[string]any:
		out := map[string]any{}
		for k, e := range x {
			if s := strip(e); s != nil {
				out[k] = s
			}
		}
		if len(out) == 0 {
			return nil
		}
		return out
	}
	return v
}

// oneOf validates a string against fixed values.
type oneOf []string

func (o oneOf) Description(context.Context) string { return "one of " + strings.Join(o, ", ") }

func (o oneOf) MarkdownDescription(ctx context.Context) string { return o.Description(ctx) }

func (o oneOf) ValidateString(_ context.Context, req validator.StringRequest, resp *validator.StringResponse) {
	if req.ConfigValue.IsNull() || req.ConfigValue.IsUnknown() {
		return
	}
	for _, v := range o {
		if req.ConfigValue.ValueString() == v {
			return
		}
	}
	resp.Diagnostics.AddAttributeError(req.Path, "Invalid value", fmt.Sprintf("give %s", strings.Join(o, " or ")))
}
