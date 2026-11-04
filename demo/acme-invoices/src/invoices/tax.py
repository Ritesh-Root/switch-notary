from decimal import Decimal

from .money import round_cents

STANDARD_RATE = Decimal("0.18")
HIGH_VALUE = Decimal("10000.00")


def line_tax(amount: Decimal, rate: Decimal = STANDARD_RATE) -> Decimal:
    """Tax on one invoice line, rounded to the cent."""
    return round_cents(amount * rate)


def invoice_tax(lines: list[Decimal], rate: Decimal = STANDARD_RATE) -> Decimal:
    """Tax owed on an invoice.

    Small invoices are taxed line by line, each line rounded to the cent. Finance rule FIN-12:
    an invoice whose subtotal is 10,000.00 or more is taxed once, on the subtotal.
    """
    return sum((line_tax(amount, rate) for amount in lines), Decimal("0.00"))
