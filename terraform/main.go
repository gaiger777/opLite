// terraform-provider-orbivisor: the Terraform/OpenTofu provider for Orbivisor (plan 73 §10).
//
// docs/ is generated from the schemas and examples/ (needs a terraform and a dev_overrides config that
// points at a built provider; the schema key is rewritten to the name tfplugindocs looks for):
//
//	terraform providers schema -json | sed 's#gaiger777/orbivisor#hashicorp/orbivisor#' > /tmp/schema.json
//	go run github.com/hashicorp/terraform-plugin-docs/cmd/tfplugindocs@v0.25.0 generate \
//	  --provider-name orbivisor --rendered-provider-name Orbivisor --providers-schema /tmp/schema.json
package main

import (
	"context"
	"flag"
	"log"

	"github.com/hashicorp/terraform-plugin-framework/providerserver"

	"github.com/gaiger777/opLite/terraform/internal/provider"
)

// version is set by the release build (-ldflags "-X main.version=…").
var version = "dev"

func main() {
	var debug bool
	flag.BoolVar(&debug, "debug", false, "run with support for debuggers like delve")
	flag.Parse()
	err := providerserver.Serve(context.Background(), provider.New(version), providerserver.ServeOpts{
		Address: "registry.terraform.io/gaiger777/orbivisor",
		Debug:   debug,
	})
	if err != nil {
		log.Fatal(err)
	}
}
