// Package datasources holds the read-only Orbivisor data sources: nodes, a storage pool, a catalog
// image, a template VM, a network and a folder.
package datasources

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/url"
	"strings"

	"github.com/hashicorp/terraform-plugin-framework/attr"
	"github.com/hashicorp/terraform-plugin-framework/datasource"
	"github.com/hashicorp/terraform-plugin-framework/datasource/schema"
	"github.com/hashicorp/terraform-plugin-framework/path"
	"github.com/hashicorp/terraform-plugin-framework/types"

	"github.com/gaiger777/opLite/terraform/internal/resources"
)

// All are the six data sources.
func All() []func() datasource.DataSource {
	return []func() datasource.DataSource{
		func() datasource.DataSource { return &nodes{} },
		func() datasource.DataSource { return &storagePool{} },
		func() datasource.DataSource { return &image{} },
		func() datasource.DataSource { return &template{} },
		func() datasource.DataSource { return &network{} },
		func() datasource.DataSource { return &folder{} },
	}
}

// base is the configure step every data source shares.
type base struct {
	data *resources.ProviderData
}

func (b *base) Configure(_ context.Context, req datasource.ConfigureRequest, resp *datasource.ConfigureResponse) {
	if req.ProviderData == nil {
		return
	}
	d, ok := req.ProviderData.(*resources.ProviderData)
	if !ok {
		resp.Diagnostics.AddError("Unexpected provider data", fmt.Sprintf("got %T", req.ProviderData))
		return
	}
	b.data = d
}

func (b *base) get(ctx context.Context, path string, out any) error {
	return b.data.Client.Get(ctx, path, out)
}

type list[T any] struct {
	Items []T `json:"items"`
}

func num(n json.Number) int64 {
	i, _ := n.Int64()
	return i
}

// ---- orbivisor_nodes

type nodes struct{ base }

type nodeModel struct {
	ID          types.String `tfsdk:"id"`
	Name        types.String `tfsdk:"name"`
	Online      types.Bool   `tfsdk:"online"`
	Arch        types.String `tfsdk:"arch"`
	CPUs        types.Int64  `tfsdk:"cpus"`
	MemoryMiB   types.Int64  `tfsdk:"memory_mib"`
	Maintenance types.Bool   `tfsdk:"maintenance"`
	Witness     types.Bool   `tfsdk:"witness"`
}

var nodeAttrTypes = map[string]attr.Type{
	"id": types.StringType, "name": types.StringType, "online": types.BoolType, "arch": types.StringType,
	"cpus": types.Int64Type, "memory_mib": types.Int64Type, "maintenance": types.BoolType, "witness": types.BoolType,
}

func (d *nodes) Metadata(_ context.Context, req datasource.MetadataRequest, resp *datasource.MetadataResponse) {
	resp.TypeName = req.ProviderTypeName + "_nodes"
}

func (d *nodes) Schema(_ context.Context, _ datasource.SchemaRequest, resp *datasource.SchemaResponse) {
	resp.Schema = schema.Schema{
		MarkdownDescription: "The cluster's nodes (names for placement rules, HA groups and per-node certificates).",
		Attributes: map[string]schema.Attribute{
			"nodes": schema.ListNestedAttribute{
				Computed: true,
				NestedObject: schema.NestedAttributeObject{Attributes: map[string]schema.Attribute{
					"id":          schema.StringAttribute{Computed: true},
					"name":        schema.StringAttribute{Computed: true},
					"online":      schema.BoolAttribute{Computed: true},
					"arch":        schema.StringAttribute{Computed: true},
					"cpus":        schema.Int64Attribute{Computed: true},
					"memory_mib":  schema.Int64Attribute{Computed: true},
					"maintenance": schema.BoolAttribute{Computed: true},
					"witness":     schema.BoolAttribute{Computed: true},
				}},
			},
			"names": schema.ListAttribute{Computed: true, ElementType: types.StringType, MarkdownDescription: "Node names in order."},
		},
	}
}

func (d *nodes) Read(ctx context.Context, _ datasource.ReadRequest, resp *datasource.ReadResponse) {
	var got list[struct {
		ID   string `json:"id"`
		Name string `json:"name"`
		Spec struct {
			Maintenance bool `json:"maintenance"`
			Witness     bool `json:"witness"`
		} `json:"spec"`
		Status struct {
			Online    bool        `json:"online"`
			Arch      string      `json:"arch"`
			CPUs      json.Number `json:"cpus"`
			MemoryMiB json.Number `json:"memory_mib"`
		} `json:"status"`
	}]
	if err := d.get(ctx, "/nodes", &got); err != nil {
		resp.Diagnostics.AddError("Cannot list nodes", err.Error())
		return
	}
	var out []nodeModel
	var names []string
	for _, n := range got.Items {
		out = append(out, nodeModel{
			ID: types.StringValue(n.ID), Name: types.StringValue(n.Name), Online: types.BoolValue(n.Status.Online),
			Arch: types.StringValue(n.Status.Arch), CPUs: types.Int64Value(num(n.Status.CPUs)),
			MemoryMiB: types.Int64Value(num(n.Status.MemoryMiB)), Maintenance: types.BoolValue(n.Spec.Maintenance),
			Witness: types.BoolValue(n.Spec.Witness),
		})
		names = append(names, n.Name)
	}
	lv, diags := types.ListValueFrom(ctx, types.ObjectType{AttrTypes: nodeAttrTypes}, out)
	resp.Diagnostics.Append(diags...)
	nv, diags := types.ListValueFrom(ctx, types.StringType, names)
	resp.Diagnostics.Append(diags...)
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("nodes"), lv)...)
	resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("names"), nv)...)
}

// ---- orbivisor_storage_pool

type storagePool struct{ base }

type poolModel struct {
	Name           types.String `tfsdk:"name"`
	ID             types.String `tfsdk:"id"`
	Backend        types.String `tfsdk:"backend"`
	Shared         types.Bool   `tfsdk:"shared"`
	Nodes          types.List   `tfsdk:"nodes"`
	TotalBytes     types.Int64  `tfsdk:"total_bytes"`
	AvailableBytes types.Int64  `tfsdk:"available_bytes"`
}

func (d *storagePool) Metadata(_ context.Context, req datasource.MetadataRequest, resp *datasource.MetadataResponse) {
	resp.TypeName = req.ProviderTypeName + "_storage_pool"
}

func (d *storagePool) Schema(_ context.Context, _ datasource.SchemaRequest, resp *datasource.SchemaResponse) {
	resp.Schema = schema.Schema{
		MarkdownDescription: "A storage pool (pools are node configuration, so they are read here rather than managed).",
		Attributes: map[string]schema.Attribute{
			"name":            schema.StringAttribute{Required: true},
			"id":              schema.StringAttribute{Computed: true},
			"backend":         schema.StringAttribute{Computed: true, MarkdownDescription: "Backend kind (`dir`, `nfs`, `lvm-thin`, `zfs`, `rbd`…)."},
			"shared":          schema.BoolAttribute{Computed: true},
			"nodes":           schema.ListAttribute{Computed: true, ElementType: types.StringType, MarkdownDescription: "Node ids that may use it (empty: all)."},
			"total_bytes":     schema.Int64Attribute{Computed: true},
			"available_bytes": schema.Int64Attribute{Computed: true},
		},
	}
}

func (d *storagePool) Read(ctx context.Context, req datasource.ReadRequest, resp *datasource.ReadResponse) {
	var m poolModel
	resp.Diagnostics.Append(req.Config.Get(ctx, &m)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var got struct {
		ID   string `json:"id"`
		Spec struct {
			Backend struct {
				Kind string `json:"kind"`
			} `json:"backend"`
			Shared bool     `json:"shared"`
			Nodes  []string `json:"nodes"`
		} `json:"spec"`
		Status struct {
			TotalBytes     *json.Number `json:"total_bytes"`
			AvailableBytes *json.Number `json:"available_bytes"`
		} `json:"status"`
	}
	if err := d.get(ctx, "/storage-pools/"+url.PathEscape(m.Name.ValueString()), &got); err != nil {
		resp.Diagnostics.AddError("Cannot read storage pool "+m.Name.ValueString(), err.Error())
		return
	}
	m.ID = types.StringValue(got.ID)
	m.Backend = types.StringValue(got.Spec.Backend.Kind)
	m.Shared = types.BoolValue(got.Spec.Shared)
	nodes, diags := types.ListValueFrom(ctx, types.StringType, append([]string{}, got.Spec.Nodes...))
	resp.Diagnostics.Append(diags...)
	m.Nodes = nodes
	m.TotalBytes = optInt(got.Status.TotalBytes)
	m.AvailableBytes = optInt(got.Status.AvailableBytes)
	resp.Diagnostics.Append(resp.State.Set(ctx, &m)...)
}

func optInt(n *json.Number) types.Int64 {
	if n == nil {
		return types.Int64Null()
	}
	return types.Int64Value(num(*n))
}

// ---- orbivisor_image

type image struct{ base }

type imageModel struct {
	ID           types.String `tfsdk:"id"`
	Arch         types.String `tfsdk:"arch"`
	Title        types.String `tfsdk:"title"`
	URL          types.String `tfsdk:"url"`
	SourceFormat types.String `tfsdk:"source_format"`
	Firmware     types.String `tfsdk:"firmware"`
	DefaultUser  types.String `tfsdk:"default_user"`
	MinSizeBytes types.Int64  `tfsdk:"min_size_bytes"`
	Volumes      types.List   `tfsdk:"volumes"`
}

func (d *image) Metadata(_ context.Context, req datasource.MetadataRequest, resp *datasource.MetadataResponse) {
	resp.TypeName = req.ProviderTypeName + "_image"
}

func (d *image) Schema(_ context.Context, _ datasource.SchemaRequest, resp *datasource.SchemaResponse) {
	resp.Schema = schema.Schema{
		MarkdownDescription: "An image catalog entry (built-in or added by an administrator) and the volumes already downloaded from it.",
		Attributes: map[string]schema.Attribute{
			"id":             schema.StringAttribute{Required: true, MarkdownDescription: "Catalog id, e.g. `ubuntu-24.04`."},
			"arch":           schema.StringAttribute{Optional: true, Computed: true, MarkdownDescription: "`x86_64` or `aarch64` (needed when the id has both)."},
			"title":          schema.StringAttribute{Computed: true},
			"url":            schema.StringAttribute{Computed: true},
			"source_format":  schema.StringAttribute{Computed: true},
			"firmware":       schema.StringAttribute{Computed: true},
			"default_user":   schema.StringAttribute{Computed: true},
			"min_size_bytes": schema.Int64Attribute{Computed: true},
			"volumes":        schema.ListAttribute{Computed: true, ElementType: types.StringType, MarkdownDescription: "Ready volumes downloaded from this image (use one as a clone source)."},
		},
	}
}

func (d *image) Read(ctx context.Context, req datasource.ReadRequest, resp *datasource.ReadResponse) {
	var m imageModel
	resp.Diagnostics.Append(req.Config.Get(ctx, &m)...)
	if resp.Diagnostics.HasError() {
		return
	}
	p := "/image-catalog"
	if a := m.Arch.ValueString(); a != "" {
		p += "?arch=" + url.QueryEscape(a)
	}
	var got list[struct {
		ID           string      `json:"id"`
		Arch         string      `json:"arch"`
		Title        string      `json:"title"`
		URL          string      `json:"url"`
		SourceFormat string      `json:"source_format"`
		Firmware     string      `json:"firmware"`
		DefaultUser  string      `json:"default_user"`
		MinSizeBytes json.Number `json:"min_size_bytes"`
		Volumes      []string    `json:"volumes"`
	}]
	if err := d.get(ctx, p, &got); err != nil {
		resp.Diagnostics.AddError("Cannot read the image catalog", err.Error())
		return
	}
	var found []int
	for i, e := range got.Items {
		if e.ID == m.ID.ValueString() {
			found = append(found, i)
		}
	}
	switch len(found) {
	case 0:
		resp.Diagnostics.AddError("Image not found", fmt.Sprintf("no catalog image %q", m.ID.ValueString()))
		return
	case 1:
	default:
		resp.Diagnostics.AddError("Image is ambiguous", fmt.Sprintf("image %q exists for several architectures; set arch", m.ID.ValueString()))
		return
	}
	e := got.Items[found[0]]
	m.Arch = types.StringValue(e.Arch)
	m.Title = types.StringValue(e.Title)
	m.URL = types.StringValue(e.URL)
	m.SourceFormat = types.StringValue(e.SourceFormat)
	m.Firmware = types.StringValue(e.Firmware)
	m.DefaultUser = types.StringValue(e.DefaultUser)
	m.MinSizeBytes = types.Int64Value(num(e.MinSizeBytes))
	vols, diags := types.ListValueFrom(ctx, types.StringType, append([]string{}, e.Volumes...))
	resp.Diagnostics.Append(diags...)
	m.Volumes = vols
	resp.Diagnostics.Append(resp.State.Set(ctx, &m)...)
}

// ---- orbivisor_template

type template struct{ base }

type templateModel struct {
	Name      types.String `tfsdk:"name"`
	ID        types.String `tfsdk:"id"`
	MemoryMiB types.Int64  `tfsdk:"memory_mib"`
	Firmware  types.String `tfsdk:"firmware"`
	Machine   types.String `tfsdk:"machine"`
	Disks     types.List   `tfsdk:"disks"`
}

func (d *template) Metadata(_ context.Context, req datasource.MetadataRequest, resp *datasource.MetadataResponse) {
	resp.TypeName = req.ProviderTypeName + "_template"
}

func (d *template) Schema(_ context.Context, _ datasource.SchemaRequest, resp *datasource.SchemaResponse) {
	resp.Schema = schema.Schema{
		MarkdownDescription: "A template VM (to clone volumes from).",
		Attributes: map[string]schema.Attribute{
			"name":       schema.StringAttribute{Required: true},
			"id":         schema.StringAttribute{Computed: true},
			"memory_mib": schema.Int64Attribute{Computed: true},
			"firmware":   schema.StringAttribute{Computed: true},
			"machine":    schema.StringAttribute{Computed: true},
			"disks":      schema.ListAttribute{Computed: true, ElementType: types.StringType, MarkdownDescription: "Volume names of its disks."},
		},
	}
}

func (d *template) Read(ctx context.Context, req datasource.ReadRequest, resp *datasource.ReadResponse) {
	var m templateModel
	resp.Diagnostics.Append(req.Config.Get(ctx, &m)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var got struct {
		ID   string `json:"id"`
		Spec struct {
			Template  bool        `json:"template"`
			MemoryMiB json.Number `json:"memory_mib"`
			Firmware  string      `json:"firmware"`
			Machine   string      `json:"machine"`
			Disks     []struct {
				Volume string `json:"volume"`
			} `json:"disks"`
		} `json:"spec"`
	}
	if err := d.get(ctx, "/vms/"+url.PathEscape(m.Name.ValueString()), &got); err != nil {
		resp.Diagnostics.AddError("Cannot read template "+m.Name.ValueString(), err.Error())
		return
	}
	if !got.Spec.Template {
		resp.Diagnostics.AddError("Not a template", fmt.Sprintf("VM %q is not a template", m.Name.ValueString()))
		return
	}
	m.ID = types.StringValue(got.ID)
	m.MemoryMiB = types.Int64Value(num(got.Spec.MemoryMiB))
	m.Firmware = types.StringValue(got.Spec.Firmware)
	m.Machine = types.StringValue(got.Spec.Machine)
	disks := []string{}
	for _, dk := range got.Spec.Disks {
		disks = append(disks, dk.Volume)
	}
	lv, diags := types.ListValueFrom(ctx, types.StringType, disks)
	resp.Diagnostics.Append(diags...)
	m.Disks = lv
	resp.Diagnostics.Append(resp.State.Set(ctx, &m)...)
}

// ---- orbivisor_network

type network struct{ base }

type networkModel struct {
	Name   types.String `tfsdk:"name"`
	ID     types.String `tfsdk:"id"`
	Folder types.String `tfsdk:"folder"`
	Bridge types.String `tfsdk:"bridge"`
	VLAN   types.Int64  `tfsdk:"vlan"`
	MTU    types.Int64  `tfsdk:"mtu"`
	SDN    types.Bool   `tfsdk:"sdn"`
}

func (d *network) Metadata(_ context.Context, req datasource.MetadataRequest, resp *datasource.MetadataResponse) {
	resp.TypeName = req.ProviderTypeName + "_network"
}

func (d *network) Schema(_ context.Context, _ datasource.SchemaRequest, resp *datasource.SchemaResponse) {
	resp.Schema = schema.Schema{
		MarkdownDescription: "A network managed elsewhere (to attach NICs to by name).",
		Attributes: map[string]schema.Attribute{
			"name":   schema.StringAttribute{Required: true},
			"id":     schema.StringAttribute{Computed: true},
			"folder": schema.StringAttribute{Computed: true, MarkdownDescription: "Folder path (empty: the root)."},
			"bridge": schema.StringAttribute{Computed: true},
			"vlan":   schema.Int64Attribute{Computed: true},
			"mtu":    schema.Int64Attribute{Computed: true},
			"sdn":    schema.BoolAttribute{Computed: true, MarkdownDescription: "An SDN (OVN) network."},
		},
	}
}

func (d *network) Read(ctx context.Context, req datasource.ReadRequest, resp *datasource.ReadResponse) {
	var m networkModel
	resp.Diagnostics.Append(req.Config.Get(ctx, &m)...)
	if resp.Diagnostics.HasError() {
		return
	}
	var got struct {
		ID string `json:"id"`
	}
	if err := d.get(ctx, "/networks/"+url.PathEscape(m.Name.ValueString()), &got); err != nil {
		resp.Diagnostics.AddError("Cannot read network "+m.Name.ValueString(), err.Error())
		return
	}
	mf, err := d.data.Client.GetManifest(ctx, "Network", m.Name.ValueString())
	if err != nil {
		resp.Diagnostics.AddError("Cannot read network "+m.Name.ValueString(), err.Error())
		return
	}
	m.ID = types.StringValue(got.ID)
	m.Folder = types.StringValue(mf.Metadata.Folder)
	b, _ := mf.Spec["bridge"].(string)
	m.Bridge = types.StringValue(b)
	m.VLAN = optInt(numberOf(mf.Spec["vlan"]))
	m.MTU = optInt(numberOf(mf.Spec["mtu"]))
	_, sdn := mf.Spec["sdn"].(map[string]any)
	m.SDN = types.BoolValue(sdn)
	resp.Diagnostics.Append(resp.State.Set(ctx, &m)...)
}

func numberOf(v any) *json.Number {
	if n, ok := v.(json.Number); ok {
		return &n
	}
	return nil
}

// ---- orbivisor_folder

type folder struct{ base }

type folderModel struct {
	Path  types.String  `tfsdk:"path"`
	Kind  types.String  `tfsdk:"kind"`
	ID    types.String  `tfsdk:"id"`
	Key   types.String  `tfsdk:"key"`
	Quota types.Dynamic `tfsdk:"quota"`
}

func (d *folder) Metadata(_ context.Context, req datasource.MetadataRequest, resp *datasource.MetadataResponse) {
	resp.TypeName = req.ProviderTypeName + "_folder"
}

func (d *folder) Schema(_ context.Context, _ datasource.SchemaRequest, resp *datasource.SchemaResponse) {
	resp.Schema = schema.Schema{
		MarkdownDescription: "A folder managed elsewhere (to put objects in by path).",
		Attributes: map[string]schema.Attribute{
			"path":  schema.StringAttribute{Required: true, MarkdownDescription: "Path, e.g. `/prod/web`."},
			"kind":  schema.StringAttribute{Optional: true, MarkdownDescription: "Tree: `vm` (default), `network` or `storage`."},
			"id":    schema.StringAttribute{Computed: true},
			"key":   schema.StringAttribute{Computed: true, MarkdownDescription: "Manifest key `tree:/path` (also a grant target)."},
			"quota": schema.DynamicAttribute{Computed: true},
		},
	}
}

func (d *folder) Read(ctx context.Context, req datasource.ReadRequest, resp *datasource.ReadResponse) {
	var m folderModel
	resp.Diagnostics.Append(req.Config.Get(ctx, &m)...)
	if resp.Diagnostics.HasError() {
		return
	}
	tree := m.Kind.ValueString()
	if tree == "" {
		tree = "vm"
	}
	p := "/" + strings.Trim(m.Path.ValueString(), "/")
	key := tree + ":" + p
	mf, err := d.data.Client.GetManifest(ctx, "Folder", key)
	if err != nil {
		resp.Diagnostics.AddError("Cannot read folder "+key, err.Error())
		return
	}
	id, err := d.folderID(ctx, tree, p)
	if err != nil {
		resp.Diagnostics.AddError("Cannot read folder "+key, err.Error())
		return
	}
	m.ID = types.StringValue(id)
	m.Key = types.StringValue(key)
	m.Kind = types.StringValue(tree)
	if q, ok := mf.Spec["quota"]; ok && q != nil {
		m.Quota = types.DynamicValue(resources.FromJSON(q))
	} else {
		m.Quota = types.DynamicNull()
	}
	resp.Diagnostics.Append(resp.State.Set(ctx, &m)...)
}

// folderID walks /folders by name and parent to the folder at path.
func (d *folder) folderID(ctx context.Context, tree, p string) (string, error) {
	var got list[struct {
		ID   string `json:"id"`
		Spec struct {
			Name   string `json:"name"`
			Kind   string `json:"kind"`
			Parent string `json:"parent"`
		} `json:"spec"`
	}]
	if err := d.get(ctx, "/folders", &got); err != nil {
		return "", err
	}
	parent := ""
	for _, part := range strings.Split(strings.Trim(p, "/"), "/") {
		next := ""
		for _, f := range got.Items {
			if f.Spec.Kind == tree && f.Spec.Name == part && f.Spec.Parent == parent {
				next = f.ID
				break
			}
		}
		if next == "" {
			return "", errors.New("not found")
		}
		parent = next
	}
	return parent, nil
}
