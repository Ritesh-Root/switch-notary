from decimal import ROUND_HALF_UP, Decimal

CENT = Decimal("0.01")


def round_cents(amount: Decimal) -> Decimal:
    """Round to the cent, halves away from zero (how Acme's bank rounds)."""
    return amount.quantize(CENT, rounding=ROUND_HALF_UP)


def fmt(amount: Decimal) -> str:
    """1234.5 -> '1,234.50'."""
    return f"{round_cents(amount):,.2f}"
