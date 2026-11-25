package provider

import (
	"testing"

	"github.com/hashicorp/terraform-plugin-framework/diag"
	"github.com/hashicorp/terraform-plugin-framework/types"
)

// Names only these tests use, so they do not meet a real ORBIVISOR_SERVER in the environment.
func TestLegacyEnv(t *testing.T) {
	t.Run("new only", func(t *testing.T) {
		t.Setenv("ORBIVISOR_LEGACY_T1", "new")
		if v, w := legacyEnv("LEGACY_T1"); v != "new" || w != "" {
			t.Fatalf("got %q %q", v, w)
		}
	})
	t.Run("old only warns", func(t *testing.T) {
		t.Setenv("OPL_LEGACY_T2", "old")
		v, w := legacyEnv("LEGACY_T2")
		if v != "old" || w != "environment variable OPL_LEGACY_T2 is deprecated; use ORBIVISOR_LEGACY_T2" {
			t.Fatalf("got %q %q", v, w)
		}
	})
	t.Run("both: new wins without a warning", func(t *testing.T) {
		t.Setenv("ORBIVISOR_LEGACY_T3", "new")
		t.Setenv("OPL_LEGACY_T3", "old")
		if v, w := legacyEnv("LEGACY_T3"); v != "new" || w != "" {
			t.Fatalf("got %q %q", v, w)
		}
	})
	t.Run("neither", func(t *testing.T) {
		if v, w := legacyEnv("LEGACY_T4_ABSENT"); v != "" || w != "" {
			t.Fatalf("got %q %q", v, w)
		}
	})
}

func TestEnvOr(t *testing.T) {
	t.Setenv("OPL_LEGACY_T5", "old")
	var diags diag.Diagnostics
	if v := envOr(types.StringValue("attr"), "LEGACY_T5", &diags); v != "attr" || diags.WarningsCount() != 0 {
		t.Fatalf("attribute: got %q %v", v, diags)
	}
	if v := envOr(types.StringNull(), "LEGACY_T5", &diags); v != "old" || diags.WarningsCount() != 1 || diags.HasError() {
		t.Fatalf("old name: got %q %v", v, diags)
	}
}
