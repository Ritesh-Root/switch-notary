# acme-invoices

The demo repository for [Agora Notary](https://github.com/Ritesh-Root/agora-notary): a small invoicing
package (tax rules and the monthly report finance reconciles against). Run the tests with `pytest -q`.

Finance rules that matter here:
- **FIN-3**: the monthly report must reconcile with what each invoice actually charged.
- **FIN-12**: invoices with a subtotal of 10,000.00 or more are taxed once, on the subtotal, rounded to
  the cent. Smaller invoices are taxed line by line.
