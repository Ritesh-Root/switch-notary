from decimal import Decimal

from invoices.tax import invoice_tax, line_tax


def test_line_tax_rounds_to_cents():
    assert line_tax(Decimal("10.03")) == Decimal("1.81")


def test_line_tax_custom_rate():
    assert line_tax(Decimal("100.00"), Decimal("0.05")) == Decimal("5.00")


def test_empty_invoice_has_no_tax():
    assert invoice_tax([]) == Decimal("0.00")


def test_small_invoice_is_taxed_line_by_line():
    # 3 x 10.03 -> 1.81 each (per line) = 5.43
    assert invoice_tax([Decimal("10.03")] * 3) == Decimal("5.43")


def test_credit_note_lines_reduce_tax():
    assert invoice_tax([Decimal("100.00"), Decimal("-40.00")]) == Decimal("10.80")


def test_just_under_threshold_is_per_line():
    lines = [Decimal("3333.33")] * 3  # 9,999.99
    assert invoice_tax(lines) == Decimal("1800.00")
