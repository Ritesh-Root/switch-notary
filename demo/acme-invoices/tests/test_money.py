from decimal import Decimal

from invoices.money import fmt, round_cents


def test_rounds_half_up():
    assert round_cents(Decimal("0.005")) == Decimal("0.01")
    assert round_cents(Decimal("0.004")) == Decimal("0.00")


def test_rounds_negative_half_away_from_zero():
    assert round_cents(Decimal("-0.005")) == Decimal("-0.01")


def test_keeps_exact_cents():
    assert round_cents(Decimal("12.34")) == Decimal("12.34")


def test_formats_with_thousands():
    assert fmt(Decimal("1234.5")) == "1,234.50"


def test_formats_zero():
    assert fmt(Decimal("0")) == "0.00"
