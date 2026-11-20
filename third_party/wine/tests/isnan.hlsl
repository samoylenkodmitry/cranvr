float4 main(float4 c : COLOR) : SV_Target
{
    float3 v = c.rgb;
    if (any(isnan(v)))
        return float4(0, 0, 0, 1);
    return float4(v, isnan(c.a) ? 0.0 : 1.0);
}
