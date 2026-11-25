// Package provider is the Orbivisor Terraform/OpenTofu provider.
package provider

import (
	"context"
	"os"
	"time"

	"github.com/hashicorp/terraform-plugin-framework/datasource"
	"github.com/hashicorp/terraform-plugin-framework/diag"
	"github.com/hashicorp/terraform-plugin-framework/path"
	"github.com/hashicorp/terraform-plugin-framework/provider"
	"github.com/hashicorp/terraform-plugin-framework/provider/schema"
	"github.com/hashicorp/terraform-plugin-framework/resource"
	"github.com/hashicorp/terraform-plugin-framework/types"

	"github.com/gaiger777/opLite/terraform/internal/client"
	"github.com/gaiger777/opLite/terraform/internal/datasources"
	"github.com/gaiger777/opLite/terraform/internal/resources"
)

// New returns the provider constructor for a version ("dev" in tests).
func New(version string) func() provider.Provider {
	return func() provider.Provider { return &orbivisor{version: version} }
}

type orbivisor struct {
	version string
}

type config struct {
	Endpoint       types.String `tfsdk:"endpoint"`
	APIToken       types.String `tfsdk:"api_token"`
	CAPEM          types.String `tfsdk:"ca_pem"`
	CAFingerprint  types.String `tfsdk:"ca_fingerprint"`
	Timeout        types.String `tfsdk:"timeout"`
	ForceConflicts types.Bool   `tfsdk:"force_conflicts"`
}

func (p *orbivisor) Metadata(_ context.Context, _ provider.MetadataRequest, resp *provider.MetadataResponse) {
	resp.TypeName = "orbivisor"
	resp.Version = p.version
}

func (p *orbivisor) Schema(_ context.Context, _ provider.SchemaRequest, resp *provider.SchemaResponse) {
	resp.Schema = schema.Schema{
		MarkdownDescription: "Manages Orbivisor clusters through the declarative manifest API (`POST /apply`). The server plans every change; `terraform plan` shows its checks (permissions, quotas, capacity, references, MAC/IP) before anything is applied.",
		Attributes: map[string]schema.Attribute{
			"endpoint": schema.StringAttribute{
				Optional:            true,
				MarkdownDescription: "API URL, e.g. `https://node1:8006` (or `ORBIVISOR_SERVER`).",
			},
			"api_token": schema.StringAttribute{
				Optional:            true,
				Sensitive:           true,
				MarkdownDescription: "API token (or `ORBIVISOR_TOKEN`). Make one with `orbivisorctl token create`.",
			},
			"ca_pem": schema.StringAttribute{
				Optional:            true,
				MarkdownDescription: "Cluster CA certificate (PEM) the API certificate is checked against (or `ORBIVISOR_CA_PEM`). Empty: the system roots (an ACME certificate).",
			},
			"ca_fingerprint": schema.StringAttribute{
				Optional:            true,
				MarkdownDescription: "Pin the cluster CA by its fingerprint `sha256:<hex>` (or `ORBIVISOR_CA_FINGERPRINT`): without `ca_pem` the CA is read from the server and accepted only with this fingerprint. The CA (read or `ca_pem`) must then be exactly one certificate.",
			},
			"timeout": schema.StringAttribute{
				Optional:            true,
				MarkdownDescription: "How long one apply or delete may take, waiting for its Task (Go duration, default `20m`).",
			},
			"force_conflicts": schema.BoolAttribute{
				Optional:            true,
				MarkdownDescription: "Take over objects an apply set (`orbivisorctl apply --set`) manages instead of failing with `managed_elsewhere`.",
			},
		},
	}
}

// envOr is the attribute, or else the environment variable ORBIVISOR_<name> (see legacyEnv). Reading the
// old name adds a warning to diags.
func envOr(v types.String, name string, diags *diag.Diagnostics) string {
	if !v.IsNull() && !v.IsUnknown() && v.ValueString() != "" {
		return v.ValueString()
	}
	value, warning := legacyEnv(name)
	if warning != "" {
		diags.AddWarning("Deprecated environment variable", warning)
	}
	return value
}

// legacyEnv reads ORBIVISOR_<name>; when it is not set, the old name OPL_<name> (accepted for one release
// after the rename, plan 74) with a deprecation warning. A set new name wins, even if empty.
func legacyEnv(name string) (value, warning string) {
	if v, ok := os.LookupEnv("ORBIVISOR_" + name); ok {
		return v, ""
	}
	if v, ok := os.LookupEnv("OPL_" + name); ok {
		return v, "environment variable OPL_" + name + " is deprecated; use ORBIVISOR_" + name
	}
	return "", ""
}

func (p *orbivisor) Configure(ctx context.Context, req provider.ConfigureRequest, resp *provider.ConfigureResponse) {
	var c config
	resp.Diagnostics.Append(req.Config.Get(ctx, &c)...)
	if resp.Diagnostics.HasError() {
		return
	}
	endpoint := envOr(c.Endpoint, "SERVER", &resp.Diagnostics)
	if endpoint == "" {
		resp.Diagnostics.AddError("Missing endpoint", "set endpoint in the provider block or ORBIVISOR_SERVER")
		return
	}
	timeout := 20 * time.Minute
	if t := c.Timeout.ValueString(); t != "" {
		d, err := time.ParseDuration(t)
		if err != nil {
			resp.Diagnostics.AddAttributeError(path.Root("timeout"), "Invalid timeout", err.Error())
			return
		}
		timeout = d
	}
	cl, err := client.New(client.Config{
		Endpoint:      endpoint,
		Token:         envOr(c.APIToken, "TOKEN", &resp.Diagnostics),
		CAPEM:         envOr(c.CAPEM, "CA_PEM", &resp.Diagnostics),
		CAFingerprint: envOr(c.CAFingerprint, "CA_FINGERPRINT", &resp.Diagnostics),
		Timeout:       timeout,
		UserAgent:     "terraform/" + p.version,
	})
	if err != nil {
		resp.Diagnostics.AddError("Cannot configure the Orbivisor client", err.Error())
		return
	}
	data := &resources.ProviderData{Client: cl, ForceConflicts: c.ForceConflicts.ValueBool()}
	resp.ResourceData = data
	resp.DataSourceData = data
}

func (p *orbivisor) Resources(context.Context) []func() resource.Resource {
	return resources.All()
}

func (p *orbivisor) DataSources(context.Context) []func() datasource.DataSource {
	return datasources.All()
}
